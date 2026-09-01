//! HULK runtime support library.
//!
//! This crate is built once as a static library (`libhulk_rt.a`) and linked
//! into every executable produced by `hulk-codegen`. Every public function is
//! `extern "C"` with a stable `#[no_mangle]` name so generated LLVM IR can
//! call it by symbol name without depending on Rust's own calling
//! conventions or name mangling.

use std::alloc::{alloc, alloc_zeroed, dealloc, Layout};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::mem::size_of;

// ─── Type tags ─────────────────────────────────────────────────────────
pub const TAG_STRING: u8 = 0;
pub const TAG_VECTOR: u8 = 1;
pub const TAG_BOX: u8 = 2;
pub const TAG_RANGE: u8 = 3;
pub const TAG_NUMBER: u8 = 4; // used inside HulkBox
pub const TAG_BOOLEAN: u8 = 5; // used inside HulkBox
pub const TAG_DYN_VEC: u8 = 6; // used for dynamic vectors (comprehensions)
pub const TAG_LITERAL_STRING: u8 = 7; // used for string literals (immutable, immortal)
pub const TAG_OBJECT: u8 = 8; // used for object instances
pub const TAG_ENV: u8 = 9; // closure environments
pub const TAG_FREED: u8 = 0xFF; // sentinel: this header was already released

/// Total number of header fields in the LLVM struct type.
pub const HEADER_FIELD_COUNT: usize = 6;
// The size of the header portion of a boxed object, in bytes.
pub const BOX_HEADER_SIZE: u64 = (8 * (HEADER_FIELD_COUNT - 1)) as u64;
/// Bytes reserved per captured-variable slot in the environment struct.
pub const ENV_SLOT_BYTES: u64 = 16;

// ─── Object header ─────────────────────────────────────────────────────
#[repr(C)]
pub struct ObjHeader {
    pub ref_count: i64,           // offset  0 — reference count
    pub gc_mark:   u8,            // offset  8 — mark bit for GC sweep
    pub type_tag:  u8,            // offset  9 — TAG_STRING / TAG_VECTOR / …
    //  [6 bytes padding to align next to 8]
    pub prev: *mut ObjHeader,     // offset 16 — intrusive alloc-list prev
    pub next: *mut ObjHeader,     // offset 24 — intrusive alloc-list next
    pub vtable: *const (),        // offset 32 — pointer to vtable array
}                                 // sizeof    = 40 bytes,

// ─── Global allocation state ───────────────────────────────────────────

/// Head of the doubly-linked intrusive allocation list.
/// Every live heap object is a node in this list.
static mut ALLOC_LIST_HEAD: *mut ObjHeader = ptr::null_mut();

/// Total bytes currently tracked in the allocation list.
static mut ALLOC_BYTES: usize = 0;

/// GC trigger threshold, read once at first allocation from the environment
/// variable `HULK_GC_THRESHOLD` (bytes). Defaults to 8 MiB.
static GC_THRESHOLD: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

fn gc_threshold() -> usize {
    *GC_THRESHOLD.get_or_init(|| {
        std::env::var("HULK_GC_THRESHOLD")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(8 * 1024 * 1024) // 8 MiB default
    })
}

// ─── Doubly-linked list helpers (runtime-internal) ────────────────────

/// Inserts `obj` at the head of the allocation list.
/// Called from hulk_rt_alloc after each successful allocation.
unsafe fn list_insert_head(obj: *mut ObjHeader) {
    (*obj).prev = ptr::null_mut();
    (*obj).next = ALLOC_LIST_HEAD;
    if !ALLOC_LIST_HEAD.is_null() {
        (*ALLOC_LIST_HEAD).prev = obj;
    }
    ALLOC_LIST_HEAD = obj;
}

/// Removes `obj` from the allocation list in O(1).
/// Called from hulk_rt_release when ref_count reaches zero and from the
/// GC sweep when an unmarked object is collected.
unsafe fn list_unlink(obj: *mut ObjHeader) {
    let prev = (*obj).prev;
    let next = (*obj).next;
    if !prev.is_null() {
        (*prev).next = next;
    } else {
        // obj was the head
        ALLOC_LIST_HEAD = next;
    }
    if !next.is_null() {
        (*next).prev = prev;
    }
    // Clear links to prevent dangling-pointer confusion during debugging.
    (*obj).next = ptr::null_mut();
    (*obj).prev = ptr::null_mut();
}

// ─── GC shadow stack ──────────────────────────────────────────────────

/// Maximum number of simultaneously live pointer-typed locals across the
/// entire call stack. 64 K entries × 8 bytes = 512 KiB of static storage.
const SHADOW_STACK_CAPACITY: usize = 65_536;

/// Each entry is a `*mut *mut c_void`: the address of a pointer-typed
/// alloca slot. The GC mark phase dereferences each entry to find the
/// current object pointer stored in that slot.
static mut SHADOW_STACK: [*mut *mut std::ffi::c_void; SHADOW_STACK_CAPACITY] =
    [ptr::null_mut(); SHADOW_STACK_CAPACITY];

/// Index of the next free slot (= current stack depth).
static mut SHADOW_TOP: usize = 0;

/// Registers the address of a pointer-typed alloca slot as a GC root.
///
/// `slot` is a `*mut ptr` — the address of an `alloca` that holds an
/// object pointer. The GC dereferences `slot` during the mark phase to
/// find the live root. Codegen calls this immediately after every
/// pointer-typed variable binding (owned or borrowed).
#[no_mangle]
pub extern "C" fn hulk_rt_shadow_push(slot: *mut std::ffi::c_void) {
    unsafe {
        debug_assert!(
            SHADOW_TOP < SHADOW_STACK_CAPACITY,
            "hulk_rt_shadow_push: shadow stack overflow"
        );
        SHADOW_STACK[SHADOW_TOP] = slot as *mut *mut std::ffi::c_void;
        SHADOW_TOP += 1;
    }
}

/// Removes the most-recently-registered GC root.
///
/// Must be called exactly once for every prior `hulk_rt_shadow_push`,
/// in reverse (LIFO) order, mirroring HULK scope exit.
#[no_mangle]
pub extern "C" fn hulk_rt_shadow_pop() {
    unsafe {
        debug_assert!(
            SHADOW_TOP > 0,
            "hulk_rt_shadow_pop: shadow stack underflow"
        );
        SHADOW_TOP -= 1;
        // Clear the slot so stale pointers do not interfere with debugging.
        SHADOW_STACK[SHADOW_TOP] = ptr::null_mut();
    }
}

// ─── Main GC logic ────────────────────────────────────────────────────────

/// Runs a full mark-sweep garbage collection cycle.
///
/// Called automatically from `hulk_rt_alloc` when the allocation byte counter
/// exceeds the threshold, and may be called explicitly in tests.
///
/// Safety: Must only be called when no object is in a partially
/// initialised state. The normal call site in `hulk_rt_alloc` 
/// guarantees this by triggering collection before allocation.
#[no_mangle]
pub extern "C" fn hulk_rt_gc_collect() {
    unsafe {
        gc_mark_roots();
        gc_sweep();
    }
}

// ─── Mark phase ────────────────────────────────────────────────────────

/// Walks the shadow stack and marks every reachable object transitively.
unsafe fn gc_mark_roots() {
    for i in 0..SHADOW_TOP {
        let slot = SHADOW_STACK[i];
        if slot.is_null() {
            continue;
        }
        // Dereference the alloca address to get the current object pointer.
        let obj_ptr = *slot as *mut ObjHeader;
        gc_mark_object(obj_ptr);
    }
}

/// Marks `obj` and recursively marks every object reachable from it.
///
/// The `gc_mark != 0` early-return makes the traversal cycle-safe: a cycle
/// of length N requires exactly N mark calls before every node is marked and
/// recursion stops.
unsafe fn gc_mark_object(obj: *mut ObjHeader) {
    if obj.is_null() || (*obj).gc_mark != 0 {
        return; // null pointer or already visited
    }
    (*obj).gc_mark = 1;

    match (*obj).type_tag {
        TAG_STRING | TAG_LITERAL_STRING => {
            // No heap-pointer fields inside a string; nothing to follow.
        }
        TAG_BOX | TAG_RANGE => {
            // Payload is a scalar (f64 or bool bitcast to i64); no pointers.
        }
        TAG_VECTOR => {
            // All len slots in the data array are object pointers by
            // uniform convention. Follow each non-null slot.
            let vec = obj as *mut HulkVector;
            let data = (*vec).data;
            let len = (*vec).len;
            for i in 0..len {
                let elem = *data.offset(i as isize) as *mut ObjHeader;
                gc_mark_object(elem);
            }
        }
        TAG_DYN_VEC => {
            let dyn_vec = obj as *mut HulkDynamicVector;
            for &elem in &(*dyn_vec).data {
                gc_mark_object(elem as *mut ObjHeader);
            }
        }
        TAG_OBJECT => {
            // vtable layout: [field_map_ptr, parent_vtable_ptr, methods…]
            // field_map layout: [size_bytes, offset_0, …, offset_N, -1]
            let vtable = (*obj).vtable as *const *const i64;
            if vtable.is_null() {
                return;
            }
            let field_map = *vtable; // vtable[0] = field map ptr
            if field_map.is_null() {
                return;
            }
            let mut i = 1isize; // skip field_map[0] (object size)
            loop {
                let offset = *field_map.offset(i);
                if offset == -1 {
                    break; // sentinel: no more pointer fields
                }
                let field_addr =
                    (obj as *mut u8).offset(offset as isize)
                    as *mut *mut ObjHeader;
                gc_mark_object(*field_addr);
                i += 1;
            }
        }
        TAG_ENV => {
            let env = obj as *mut HulkEnv;
            let field_map = (*obj).vtable as *const i64;
            if field_map.is_null() || (*env).data.is_null() {
                return;
            }
            let mut i = 0isize;
            loop {
                let offset = *field_map.offset(i);
                if offset == -1 {
                    break; // sentinel: no more pointer slots
                }
                let field_addr =
                    (*env).data.offset(offset as isize)
                    as *mut *mut ObjHeader;
                gc_mark_object(*field_addr);
                i += 1;
            }
        }
        _ => {
            // Unknown tag: conservatively do not follow any fields.
            // The object will survive if it is in the allocation list
            // and was reached from a root; otherwise sweep frees it.
        }
    }
}

// ─── Sweep phase ───────────────────────────────────────────────────────

/// Walks the entire allocation list, freeing every unmarked object and
/// clearing the mark bit on every survivor.
///
/// Safety: After `gc_free_object` the node's memory is returned to the allocator; 
/// reading `(*current).next` after that is undefined behaviour.
/// Saving `next` first is mandatory.
unsafe fn gc_sweep() {
    let mut current = ALLOC_LIST_HEAD;
    while !current.is_null() {
        let next = (*current).next; // save before potential free

        if (*current).gc_mark == 0 {
            // Unmarked: unreachable (cycle member or leaked). Free it.
            list_unlink(current);
            ALLOC_BYTES = ALLOC_BYTES.saturating_sub(size_of_object(current));
            gc_free_object(current); // hulk_rt_release is not called here:
            // Release cascades into children and decrements their ref_counts, but 
            // children that are also part of the cycle are themselves in the allocation
            // list and will be swept in this same pass. Double-freeing them would corrupt 
            // the heap. The sweep frees each object exactly once by walking the list linearly.
        } else {
            // Marked survivor: clear mark bit for next collection cycle.
            (*current).gc_mark = 0;
        }

        current = next;
    }
}

/// Returns the allocation size of `obj` in bytes by reading from the object's
/// field map (for TAG_OBJECT) or from the known fixed layout (for built-in types).
/// Used to update ALLOC_BYTES on deallocation.
unsafe fn size_of_object(obj: *mut ObjHeader) -> usize {
    match (*obj).type_tag {
        TAG_STRING => size_of::<HulkString>(),
        TAG_VECTOR => size_of::<HulkVector>(),
        TAG_BOX => size_of::<HulkBox>(),
        TAG_RANGE => size_of::<HulkRange>(),
        TAG_DYN_VEC => size_of::<HulkDynamicVector>(),
        TAG_OBJECT => {
            let vtable = (*obj).vtable as *const *const i64;
            if vtable.is_null() { return BOX_HEADER_SIZE as usize; }
            let field_map = *vtable;
            if field_map.is_null() { return BOX_HEADER_SIZE as usize; }
            *field_map as usize // field_map[0] = size
        }
        TAG_ENV => size_of::<HulkEnv>(),
        _ => BOX_HEADER_SIZE as usize, // conservative fallback
    }
}

/// Frees the raw memory of `obj` without releasing its children.
///
/// Used exclusively by the sweep phase. Children of collected cycle members
/// are handled by the sweep's own linear traversal — cascading release here
/// would double-free them.
unsafe fn gc_free_object(obj: *mut ObjHeader) {
    match (*obj).type_tag {
        TAG_STRING => {
            let s = obj as *mut HulkString;
            let len = (*s).len as usize;
            if len > 0 && !(*s).data.is_null() {
                dealloc((*s).data, Layout::array::<u8>(len).unwrap());
            }
            dealloc(s as *mut u8, Layout::new::<HulkString>());
        }
        TAG_VECTOR => {
            let vec = obj as *mut HulkVector;
            let len = (*vec).len as usize;
            if !(*vec).data.is_null() && len > 0 {
                unsafe { ALLOC_BYTES = ALLOC_BYTES.saturating_sub(len * std::mem::size_of::<*mut std::ffi::c_void>()); }
                dealloc(
                    (*vec).data as *mut u8,
                    Layout::array::<*mut std::ffi::c_void>(len).unwrap(),
                );
            }
            dealloc(vec as *mut u8, Layout::new::<HulkVector>());
        }
        TAG_BOX => {
            dealloc(obj as *mut u8, Layout::new::<HulkBox>());
        }
        TAG_RANGE => {
            dealloc(obj as *mut u8, Layout::new::<HulkRange>());
        }
        TAG_DYN_VEC => {
            let dyn_vec = obj as *mut HulkDynamicVector;
            // Drop the Vec's buffer (the sweep will handle the elements separately).
            ptr::drop_in_place(&mut (*dyn_vec).data);
            // Deallocate the struct (already unlinked by gc_sweep).
            let layout = Layout::new::<HulkDynamicVector>();
            dealloc(dyn_vec as *mut u8, layout);
        }
        TAG_OBJECT => {
            let vtable = (*obj).vtable as *const *const i64;
            if !vtable.is_null() {
                let field_map = *vtable;
                if !field_map.is_null() {
                    let obj_size = *field_map as usize;
                    let layout = Layout::from_size_align(obj_size, 8)
                        .unwrap_or_else(|_| Layout::from_size_align(BOX_HEADER_SIZE as usize, 8).unwrap());
                    dealloc(obj as *mut u8, layout);
                    return;
                }
            }
            // Fallback: vtable or field map missing; use minimum header size.
            dealloc(obj as *mut u8, Layout::from_size_align(BOX_HEADER_SIZE as usize, 8).unwrap());
        }
        TAG_ENV => {
            let env = obj as *mut HulkEnv;
            if !(*env).data.is_null() {
                let byte_len = (*env).slot_count as usize * ENV_SLOT_BYTES as usize;
                if byte_len > 0 {
                    dealloc((*env).data, Layout::array::<u8>(byte_len).unwrap());
                }
            }
            dealloc(env as *mut u8, Layout::new::<HulkEnv>());
        }
        _ => {
            dealloc(obj as *mut u8, Layout::from_size_align(BOX_HEADER_SIZE as usize, 8).unwrap());
        }
    }
}

// ─── HulkString ────────────────────────────────────────────────────────
#[repr(C)]
pub struct HulkString {
    pub header: ObjHeader,
    pub len: i64,
    pub data: *mut u8,
}

// ─── HulkVector ────────────────────────────────────────────────────────
#[repr(C)]
pub struct HulkVector {
    pub header: ObjHeader,
    pub len: i64,
    pub current_index: i64, // For iteration
    pub data: *mut *mut std::ffi::c_void,
}

// ─── Dynamic vector (for comprehensions) ──────────────────────────────
#[repr(C)]
pub struct HulkDynamicVector {
    pub header: ObjHeader,
    pub data: Vec<*mut std::ffi::c_void>,
}

// ─── HulkBox ───────────────────────────────────────────────────────────
#[repr(C)]
pub struct HulkBox {
    pub header: ObjHeader,
    pub original_tag: u8, // TAG_NUMBER, TAG_BOOLEAN
    pub _padding: [u8; 7],
    pub payload: i64, // bitcast of f64 or bool (as i64)
}

// ─── HulkRange ─────────────────────────────────────────────────────────
#[repr(C)]
pub struct HulkRange {
    pub header: ObjHeader,
    pub min: f64,
    pub max: f64,
    pub current: f64,
}

// ─── Closure environment ───────────────────────────────────────────────

/// Holds the values captured by a closure at creation time.
#[repr(C)]
pub struct HulkEnv {
    pub header: ObjHeader,
    pub slot_count: i64,
    pub data: *mut u8,
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// Allocates a HulkString from a byte slice.
unsafe fn hulk_rt_string_from_bytes(data: &[u8]) -> *mut HulkString {
    let len = data.len() as i64;
    let string_layout = Layout::new::<HulkString>();
    // Allocate the HulkString struct via hulk_rt_alloc so it is tracked by the GC.
    let string_ptr = hulk_rt_alloc(string_layout.size() as i64) as *mut HulkString;
    if string_ptr.is_null() {
        return ptr::null_mut();
    }

    let data_layout = Layout::array::<u8>(len as usize).unwrap();
    let data_ptr = alloc(data_layout);
    if data_ptr.is_null() {
        // Unlink the struct from the allocation list before deallocating.
        list_unlink(string_ptr as *mut ObjHeader);
        dealloc(string_ptr as *mut u8, string_layout);
        return ptr::null_mut();
    }
    ptr::copy(data.as_ptr(), data_ptr, len as usize);

    // Set header fields individually; do NOT overwrite prev/next.
    (*string_ptr).header.ref_count = 1;
    (*string_ptr).header.gc_mark = 0;
    (*string_ptr).header.type_tag = TAG_STRING;
    (*string_ptr).header.vtable = ptr::null();
    // prev and next are already set by list_insert_head; leave them untouched.
    (*string_ptr).len = len;
    (*string_ptr).data = data_ptr;

    string_ptr
}

fn is_immortal_ptr(ptr: *mut std::ffi::c_void) -> bool {
    if ptr.is_null() {
        return false;
    }
    unsafe {
        let header = ptr as *mut ObjHeader;
        (*header).type_tag == TAG_LITERAL_STRING
    }
}

fn _is_immortal_header(header: *mut ObjHeader) -> bool {
    if header.is_null() {
        return false;
    }
    unsafe { (*header).type_tag == TAG_LITERAL_STRING }
}

// ─── Base runtime functions ──────────────────────────────────────────────

/// A no-op function that does nothing.
///
/// Used as a placeholder to test that the runtime is correctly linked.
#[no_mangle]
pub extern "C" fn hulk_rt_noop() {}

/// Concatenates two HULK strings without adding a separator.
///
/// # Parameters
/// - `a`: Pointer to the first `HulkString` object.
/// - `b`: Pointer to the second `HulkString` object.
///
/// # Returns
/// A pointer to a newly allocated `HulkString` object that contains the
/// concatenation of `a` followed by `b`.
///
/// # Safety
/// The caller must ensure that both pointers point to valid `HulkString`
/// objects, and that the strings are immutable (no mutation during the call).
#[no_mangle]
pub extern "C" fn hulk_rt_string_concat(
    a: *mut std::ffi::c_void,
    b: *mut std::ffi::c_void,
) -> *mut std::ffi::c_void {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        let sa = a as *mut HulkString;
        let sb = b as *mut HulkString;
        let len_a = (*sa).len as usize;
        let len_b = (*sb).len as usize;
        let total_len = len_a + len_b;
        let mut buf = Vec::with_capacity(total_len);
        buf.extend_from_slice(std::slice::from_raw_parts((*sa).data, len_a));
        buf.extend_from_slice(std::slice::from_raw_parts((*sb).data, len_b));
        hulk_rt_string_from_bytes(&buf) as *mut std::ffi::c_void
    }
}

/// Concatenates two HULK strings with a single space inserted between them.
///
/// # Parameters
/// - `a`: Pointer to the first `HulkString` object.
/// - `b`: Pointer to the second `HulkString` object.
///
/// # Returns
/// A pointer to a newly allocated `HulkString` object that contains the
/// concatenation of `a`, a literal space, and `b`.
///
/// # Safety
/// The caller must ensure that both pointers point to valid `HulkString`
/// objects.
#[no_mangle]
pub extern "C" fn hulk_rt_string_concat_space(
    a: *mut std::ffi::c_void,
    b: *mut std::ffi::c_void,
) -> *mut std::ffi::c_void {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        let sa = a as *mut HulkString;
        let sb = b as *mut HulkString;
        let len_a = (*sa).len as usize;
        let len_b = (*sb).len as usize;
        let total_len = len_a + 1 + len_b;
        let mut buf = Vec::with_capacity(total_len);
        buf.extend_from_slice(std::slice::from_raw_parts((*sa).data, len_a));
        buf.push(b' ');
        buf.extend_from_slice(std::slice::from_raw_parts((*sb).data, len_b));
        hulk_rt_string_from_bytes(&buf) as *mut std::ffi::c_void
    }
}

/// Returns true if two HULK strings have identical byte content.
///
/// # Safety
/// Both pointers must point to valid `HulkString` objects.
#[no_mangle]
pub extern "C" fn hulk_rt_string_equals(
    a: *mut std::ffi::c_void,
    b: *mut std::ffi::c_void,
) -> bool {
    if a.is_null() && b.is_null() {
        return true;
    }
    if a.is_null() || b.is_null() {
        return false;
    }
    unsafe {
        let sa = a as *mut HulkString;
        let sb = b as *mut HulkString;
        let len_a = (*sa).len as usize;
        let len_b = (*sb).len as usize;
        if len_a != len_b {
            return false;
        }
        let slice_a = std::slice::from_raw_parts((*sa).data, len_a);
        let slice_b = std::slice::from_raw_parts((*sb).data, len_b);
        slice_a == slice_b
    }
}

/// Converts a 64-bit floating-point number to its string representation.
///
/// # Parameters
/// - `num`: The number to convert.
///
/// # Returns
/// A pointer to a newly allocated `HulkString` object containing the
/// decimal representation of `num`.
#[no_mangle]
pub extern "C" fn hulk_rt_number_to_string(num: f64) -> *mut std::ffi::c_void {
    let s = num.to_string();
    unsafe { hulk_rt_string_from_bytes(s.as_bytes()) as *mut std::ffi::c_void }
}

/// Converts a boolean value to its string representation.
///
/// # Parameters
/// - `b`: The boolean value to convert (0 = false, 1 = true).
///
/// # Returns
/// A pointer to a newly allocated `HulkString` object containing `"true"` or
/// `"false"`.
#[no_mangle]
pub extern "C" fn hulk_rt_bool_to_string(b: bool) -> *mut std::ffi::c_void {
    let s = if b { "true" } else { "false" };
    unsafe { hulk_rt_string_from_bytes(s.as_bytes()) as *mut std::ffi::c_void }
}

/// Prints a HULK object to standard output.
#[no_mangle]
pub extern "C" fn hulk_rt_print(obj: *mut std::ffi::c_void) -> *mut std::ffi::c_void {
    use std::io::Write;
    if obj.is_null() {
        println!("null");
        return obj;
    }
    unsafe {
        let header = obj as *mut ObjHeader;
        match (*header).type_tag {
            TAG_STRING | TAG_LITERAL_STRING => {
                let s = obj as *mut HulkString;
                let len = (*s).len as usize;
                let data = std::slice::from_raw_parts((*s).data, len);
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(data);
                let _ = out.write_all(b"\n");
            }
            TAG_BOX => {
                let boxed = obj as *mut HulkBox;
                match (*boxed).original_tag {
                    TAG_NUMBER => {
                        let val = f64::from_bits((*boxed).payload as u64);
                        println!("{}", val);
                    }
                    TAG_BOOLEAN => {
                        let val = (*boxed).payload != 0;
                        println!("{}", val);
                    }
                    _ => println!("<unknown box>"),
                }
            }
            TAG_VECTOR => {
                let vec = obj as *mut HulkVector;
                let len = (*vec).len;
                print!("[");
                for i in 0..len {
                    let elem = hulk_rt_vector_get(vec, i);
                    if i > 0 {
                        print!(", ");
                    }
                    if elem.is_null() {
                        print!("null");
                    } else {
                        // For simplicity, print the address; can be improved later.
                        print!("<obj@{:p}>", elem);
                    }
                }
                println!("]");
            }
            _ => println!("<object>"),
        }
    }
    obj
}

// ─── Memory management ─────────────────────────────────────────────────

/// Allocates a block of memory of the given size in bytes.
///
/// # Parameters
/// - `size`: The number of bytes to allocate. Must be greater than zero.
///
/// # Returns
/// A pointer to the newly allocated memory block, or a null pointer if
/// allocation fails. The allocated memory is zero-initialised (guaranteed by
/// `std::alloc`).
///
/// # Safety
/// This function is safe to call from any context, but the caller is
/// responsible for freeing the allocated memory via the corresponding
/// deallocation function.
#[no_mangle]
pub extern "C" fn hulk_rt_alloc(size: i64) -> *mut std::ffi::c_void {
    if size <= 0 {
        return ptr::null_mut();
    }
    let byte_count = size as usize;

    // ── 1. Threshold check — trigger GC before allocating ────────────
    // Collect first so the new object is not in the list during the mark phase. 
    // This is the only safe window; after insertion the mark phase may encounter 
    // a partially initialised header.
    unsafe {
        if ALLOC_BYTES.saturating_add(byte_count) > gc_threshold() {
            hulk_rt_gc_collect();
        }
    }

    // ── 2. Allocate zeroed memory ─────────────────────────────────────
    let layout = match Layout::from_size_align(byte_count, 8) {
        Ok(l) => l,
        Err(_) => return ptr::null_mut(),
    };
    let raw = unsafe { alloc_zeroed(layout) };
    if raw.is_null() {
        return ptr::null_mut();
    }

    // ── 3. Link into the allocation list and update byte counter ──────
    // The memory is zero-initialised, which is a safe sentinel state
    // (ref_count=0, gc_mark=0, tag=TAG_STRING, vtable=null).
    unsafe {
        let header = raw as *mut ObjHeader;
        list_insert_head(header);
        ALLOC_BYTES = ALLOC_BYTES.saturating_add(byte_count);
    }

    raw as *mut std::ffi::c_void
}

/// Increments the reference count of an object pointed to by `ptr`.
///
/// Useful for managing the lifetime of objects in a reference-counted memory model.
#[no_mangle]
pub extern "C" fn hulk_rt_retain(ptr: *mut std::ffi::c_void) {
    if ptr.is_null() || is_immortal_ptr(ptr)
    // Retain is a no-op for immortal types.
    {
        return;
    }
    unsafe {
        let header = ptr as *mut ObjHeader;
        // ref_count == -1 is the immortal sentinel (CPython PEP 683 pattern).
        // String literals live in read-only .rodata; attempting to write would SIGSEGV.
        if (*header).ref_count == -1 {
            return;
        }
        (*header).ref_count += 1;
    }
}

/// Decrements the reference count of an object pointed to by `ptr`.
/// If the reference count reaches zero, the object is deallocated.
/// 
/// Nullification of `ptr`` after release is strongly recommended.
#[no_mangle]
pub extern "C" fn hulk_rt_release(ptr: *mut std::ffi::c_void) {
    if ptr.is_null() || is_immortal_ptr(ptr) {
        return;
    }
    unsafe {
        let header = ptr as *mut ObjHeader;
        if (*header).type_tag == TAG_FREED {
            #[cfg(debug_assertions)]
            panic!("hulk_rt_release: use-after-free / double-release on {:p}", ptr);
            #[cfg(not(debug_assertions))]
            return; // fail safe in release builds: no-op instead of corruption
        }
        if (*header).ref_count == -1 {
            return; // immortal sentinel
        }
        (*header).ref_count -= 1;
        if (*header).ref_count != 0 {
            return;
        }

        // ref_count reached zero: unlink from alloc list first (O(1)),
        // then dispatch to the type-specific destructor.
        list_unlink(header);
        ALLOC_BYTES = ALLOC_BYTES.saturating_sub(size_of_object(header));

        let tag = (*header).type_tag;
        (*header).type_tag = TAG_FREED;
        (*header).ref_count = i64::MIN; // implausible for any live object

        match tag {
            TAG_STRING => {
                let s = ptr as *mut HulkString;
                let len = (*s).len as usize;
                if len > 0 && !(*s).data.is_null() {
                    let data_layout = Layout::array::<u8>(len).unwrap();
                    dealloc((*s).data, data_layout);
                }
                dealloc(s as *mut u8, Layout::new::<HulkString>());
            }
            TAG_VECTOR => {
                let vec = ptr as *mut HulkVector;
                let data = (*vec).data;
                let len = (*vec).len as usize;
                // Release each element before freeing the array.
                for i in 0..len {
                    let elem = *data.offset(i as isize);
                    if !elem.is_null() {
                        hulk_rt_release(elem);
                    }
                }
                let data_layout = Layout::array::<*mut std::ffi::c_void>(len).unwrap();
                ALLOC_BYTES = ALLOC_BYTES.saturating_sub(len * std::mem::size_of::<*mut std::ffi::c_void>());
                dealloc(data as *mut u8, data_layout);
                dealloc(vec as *mut u8, Layout::new::<HulkVector>());
            }
            TAG_BOX => {
                dealloc(ptr as *mut u8, Layout::new::<HulkBox>());
            }
            TAG_RANGE => {
                dealloc(ptr as *mut u8, Layout::new::<HulkRange>());
            }
            TAG_DYN_VEC => {
                let dyn_vec = ptr as *mut HulkDynamicVector;
                // Release each element (the sweep does not cascade).
                for &elem in &(*dyn_vec).data {
                    if !elem.is_null() {
                        hulk_rt_release(elem);
                    }
                }
                // Drop the Vec's internal buffer.
                ptr::drop_in_place(&mut (*dyn_vec).data);
                // Deallocate the struct (already unlinked above).
                let layout = Layout::new::<HulkDynamicVector>();
                dealloc(dyn_vec as *mut u8, layout);
            }
            TAG_OBJECT => {
                // ── Read object size and pointer-field offsets from field map ──
                // vtable layout: [field_map_ptr, parent_vtable_ptr, methods…]
                // field_map layout: [size_bytes, offset_0, …, offset_N, -1]
                let vtable = (*header).vtable as *const *const i64;
                if !vtable.is_null() {
                    let field_map = *vtable; // vtable[0] = field map
                    if !field_map.is_null() {
                        let obj_size = *field_map as usize; // field_map[0] = size

                        // Release each pointer-typed child field.
                        let mut i = 1isize; // start at index 1, skip size
                        loop {
                            let offset = *field_map.offset(i);
                            if offset == -1 { break; }
                            let field_addr =
                                (ptr as *mut u8).offset(offset as isize)
                                as *mut *mut std::ffi::c_void;
                            let child = *field_addr;
                            if !child.is_null() {
                                hulk_rt_release(child);
                            }
                            i += 1;
                        }

                        // Free the object header + fields in one allocation.
                        let layout = Layout::from_size_align(obj_size, 8)
                            .unwrap_or_else(|_| Layout::from_size_align(BOX_HEADER_SIZE as usize, 8).unwrap());
                        dealloc(ptr as *mut u8, layout);
                    }
                }
            }
            TAG_ENV => {
                let env = ptr as *mut HulkEnv;
                let field_map = (*header).vtable as *const i64;
                if !field_map.is_null() && !(*env).data.is_null() {
                    let mut i = 0isize;
                    loop {
                        let offset = *field_map.offset(i);
                        if offset == -1 {
                            break;
                        }
                        let slot = (*env).data.offset(offset as isize) as *mut *mut std::ffi::c_void;
                        if !(*slot).is_null() {
                            hulk_rt_release(*slot);
                        }
                        i += 1;
                    }
                }
                if !(*env).data.is_null() {
                    let byte_len = (*env).slot_count as usize * ENV_SLOT_BYTES as usize;
                    if byte_len > 0 {
                        ALLOC_BYTES = ALLOC_BYTES.saturating_sub(byte_len);
                        dealloc((*env).data, Layout::array::<u8>(byte_len).unwrap());
                    }
                }
                dealloc(env as *mut u8, Layout::new::<HulkEnv>());
            }
            _ => {
                // Unknown tag: object was allocated by hulk_rt_alloc but its
                // tag was never set to a known value. Free with the minimum
                // header size to avoid a leak; log in debug builds.
                debug_assert!(false, "hulk_rt_release: unknown type_tag {}", (*header).type_tag);
                let layout = Layout::from_size_align(BOX_HEADER_SIZE as usize, 8).unwrap();
                dealloc(ptr as *mut u8, layout);
            }
        }
    }
}

// ─── Vector Functions ──────────────────────────────────────────────────

/// Allocates a HulkVector and its data array, sets initial refcount to 1, and returns the pointer.
#[no_mangle]
pub extern "C" fn hulk_rt_vector_new(len: i64) -> *mut HulkVector {
    if len < 0 {
        return ptr::null_mut();
    }

    // 1. Allocate the HulkVector struct itself.
    let vec_layout = Layout::new::<HulkVector>();
    let vec_ptr = hulk_rt_alloc(vec_layout.size() as i64) as *mut HulkVector;
    if vec_ptr.is_null() {
        return ptr::null_mut();
    }

    // 2. Allocate the data array (len pointers).
    let data_ptr: *mut *mut std::ffi::c_void = if len == 0 {
        ptr::NonNull::dangling().as_ptr()  // valid non-null sentinel, never dereferenced
    } else {
        let data_layout = Layout::array::<*mut std::ffi::c_void>(len as usize)
            .expect("vector data layout overflow");
        let raw = unsafe { alloc_zeroed(data_layout) } as *mut *mut std::ffi::c_void;
        if raw.is_null() {
            // cleanup vec_ptr and return null
            unsafe {
                list_unlink(vec_ptr as *mut ObjHeader);
                dealloc(vec_ptr as *mut u8, vec_layout);
            }
            return ptr::null_mut();
        }
        raw  // alloc_zeroed already zeroes the data, no write_bytes needed
    };
    let data_byte_count = (len as usize) * std::mem::size_of::<*mut std::ffi::c_void>();
    unsafe { ALLOC_BYTES = ALLOC_BYTES.saturating_add(data_byte_count); }

    // 3. Zero-initialise the data array.
    unsafe {
        ptr::write_bytes(data_ptr, 0, len as usize);
    }

    // 4. Fill the vector fields.
    unsafe {
        // Set fields individually; preserve prev/next from list_insert_head.
        (*vec_ptr).header.ref_count = 1;
        (*vec_ptr).header.gc_mark = 0;
        (*vec_ptr).header.type_tag = TAG_VECTOR;
        (*vec_ptr).header.vtable = ptr::null();
        (*vec_ptr).len = len;
        (*vec_ptr).current_index = -1;
        (*vec_ptr).data = data_ptr;
    }

    vec_ptr
}

/// Returns the number of elements in a HulkVector.
///
/// # Safety
/// `vec` must be a valid, aligned pointer to a live `HulkVector`.
#[no_mangle]
pub unsafe extern "C" fn hulk_rt_vector_size(vec: *mut HulkVector) -> f64 {
    if vec.is_null() {
        0.0
    } else {
        (*vec).len as f64
    }
}

/// Retrieves the element at the given index from a HulkVector.
///
/// Note: This function does not perform bounds checking; the caller must ensure that `index` is valid.
///
/// # Safety
/// `vec` must be null or a valid, aligned pointer to a live `HulkVector`.
#[no_mangle]
pub unsafe extern "C" fn hulk_rt_vector_get(
    vec: *mut HulkVector,
    index: i64,
) -> *mut std::ffi::c_void {
    if vec.is_null() || index < 0 {
        return ptr::null_mut();
    }
    unsafe {
        let data = (*vec).data;
        let len = (*vec).len;
        if index >= len {
            return ptr::null_mut();
        }
        *data.offset(index as isize)
    }
}

/// Sets the element at the given index in a HulkVector to a new value, managing reference counts appropriately.
///
/// # Safety
/// `vec` must be null or a valid, aligned pointer to a live `HulkVector`.
#[no_mangle]
pub unsafe extern "C" fn hulk_rt_vector_set(
    vec: *mut HulkVector,
    index: i64,
    value: *mut std::ffi::c_void,
) {
    if vec.is_null() || index < 0 {
        return;
    }
    unsafe {
        let data = (*vec).data;
        let len = (*vec).len;
        if index >= len {
            return;
        }

        let slot = data.offset(index as isize);
        let old = *slot;

        // Release the old value (if any)
        if !old.is_null() {
            hulk_rt_release(old);
        }

        // Store the new value and retain it (if non‑null)
        if !value.is_null() {
            hulk_rt_retain(value);
        }
        *slot = value;
    }
}

/// Advances the current index of the vector for iteration.
///
/// Returns true if there is a next element, false otherwise.
///
/// # Safety
/// `vec` must be null or a valid, aligned pointer to a live `HulkVector`.
#[no_mangle]
pub unsafe extern "C" fn hulk_rt_vector_next(vec: *mut HulkVector) -> bool {
    if vec.is_null() {
        return false;
    }
    unsafe {
        let idx = (*vec).current_index + 1; // Vectors are 0-indexed
        if idx < (*vec).len {
            (*vec).current_index = idx;
            true
        } else {
            false
        }
    }
}

/// Returns the current element in the vector based on the current index.
///
/// Note: The caller must ensure that `hulk_rt_vector_next` has been called
/// and returned true before calling this function.
///
/// # Safety
/// `vec` must be null or a valid, aligned pointer to a live `HulkVector`.
#[no_mangle]
pub unsafe extern "C" fn hulk_rt_vector_current(vec: *mut HulkVector) -> *mut std::ffi::c_void {
    if vec.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        let idx = (*vec).current_index;
        let pos = idx;
        if pos < 0 || pos >= (*vec).len {
            return ptr::null_mut();
        }
        let data = (*vec).data;
        *data.offset(pos as isize)
    }
}

// ─── Dynamic vector helpers for comprehensions ────────────────────────

#[no_mangle]
pub extern "C" fn hulk_rt_dynamic_vector_new() -> *mut HulkDynamicVector {
    let layout = Layout::new::<HulkDynamicVector>();
    let ptr = hulk_rt_alloc(layout.size() as i64) as *mut HulkDynamicVector;
    if ptr.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        (*ptr).header.ref_count = 1;
        (*ptr).header.gc_mark = 0;
        (*ptr).header.type_tag = TAG_DYN_VEC;
        (*ptr).header.vtable = ptr::null();
        (*ptr).data = Vec::new();
    }
    ptr
}

/// Appends a value to a dynamic vector, retaining it.
///
/// # Safety
/// `vec` must be null or a valid, aligned pointer to a live `HulkDynamicVector`.
#[no_mangle]
pub unsafe extern "C" fn hulk_rt_dynamic_vector_append(
    vec: *mut HulkDynamicVector,
    value: *mut std::ffi::c_void,
) {
    if vec.is_null() {
        return;
    }
    unsafe {
        let vec_ref = &mut *vec;
        vec_ref.data.push(value);
        if !value.is_null() {
            hulk_rt_retain(value);
        }
    }
}

/// Converts a dynamic vector into a fixed-size `HulkVector`, consuming the dynamic vector.
///
/// # Safety
/// `dyn_vec` must be null or a valid, aligned pointer to a live `HulkDynamicVector`.
#[no_mangle]
pub unsafe extern "C" fn hulk_rt_dynamic_vector_to_vector(
    dyn_vec: *mut HulkDynamicVector,
) -> *mut HulkVector {
    if dyn_vec.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        let vec_ref = &mut *dyn_vec;
        let len = vec_ref.data.len() as i64;
        let fixed = hulk_rt_vector_new(len);
        if fixed.is_null() {
            // Clean up: drop Vec buffer and deallocate struct.
            ptr::drop_in_place(&mut (*dyn_vec).data);
            list_unlink(dyn_vec as *mut ObjHeader);
            let layout = Layout::new::<HulkDynamicVector>();
            dealloc(dyn_vec as *mut u8, layout);
            return ptr::null_mut();
        }
        for (i, &val) in vec_ref.data.iter().enumerate() {
            hulk_rt_vector_set(fixed, i as i64, val);
            if !val.is_null() {
                // Give up the ownership stake `hulk_rt_dynamic_vector_append` acquired;
                // `fixed` now holds the sole retained reference from this conversion.
                hulk_rt_release(val);
            }
        }
        // Clean up: drop the Vec's buffer and deallocate the struct.
        ptr::drop_in_place(&mut (*dyn_vec).data);
        list_unlink(dyn_vec as *mut ObjHeader);
        let layout = Layout::new::<HulkDynamicVector>();
        dealloc(dyn_vec as *mut u8, layout);
        fixed
    }
}

// ─── Range Functions ──────────────────────────────────────────────────

/// Creates a new HulkRange object with the specified minimum and maximum values.
#[no_mangle]
pub extern "C" fn hulk_rt_range_new(min: f64, max: f64) -> *mut HulkRange {
    let layout = Layout::new::<HulkRange>();
    let ptr = hulk_rt_alloc(layout.size() as i64) as *mut HulkRange;
    if ptr.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        (*ptr).header.ref_count = 1;
        (*ptr).header.gc_mark = 0;
        (*ptr).header.type_tag = TAG_RANGE;
        (*ptr).header.vtable = ptr::null();
        (*ptr).min = min;
        (*ptr).max = max;
        (*ptr).current = min - 1.0;
    }
    ptr
}

/// Advances the current value of the range for iteration.
///
/// Returns true if there is a next value, false otherwise.
///
/// # Safety
/// `rng` must be null or a valid, aligned pointer to a live `HulkRange`.
#[no_mangle]
pub unsafe extern "C" fn hulk_rt_range_next(rng: *mut HulkRange) -> bool {
    if rng.is_null() {
        return false;
    }
    unsafe {
        (*rng).current += 1.0;
        (*rng).current < (*rng).max
    }
}

/// Returns the current value of the range.
///
/// Note: The caller must ensure that `hulk_rt_range_next` has been called
/// and returned true before calling this function.
///
/// # Safety
/// `rng` must be null or a valid, aligned pointer to a live `HulkRange`.
#[no_mangle]
pub unsafe extern "C" fn hulk_rt_range_current(rng: *mut HulkRange) -> f64 {
    if rng.is_null() {
        return 0.0;
    }
    unsafe { (*rng).current }
}

// ─── Closure Environments ──────────────────────────────────────────────────

/// Allocates a closure environment with `slot_count` capture slots of
/// `ENV_SLOT_BYTES` each, tagged with `field_map` for GC tracing (see
/// `HulkEnv`). Returns a fully-initialised, ref_count = 1 object.
#[no_mangle]
pub extern "C" fn hulk_rt_env_new(
    slot_count: i64,
    field_map: *const i64,
) -> *mut std::ffi::c_void {
    if slot_count < 0 {
        return ptr::null_mut();
    }
    let struct_size = std::mem::size_of::<HulkEnv>() as i64;
    let env_ptr = hulk_rt_alloc(struct_size) as *mut HulkEnv;
    if env_ptr.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        (*env_ptr).header.type_tag = TAG_ENV;
        (*env_ptr).header.ref_count = 1;
        (*env_ptr).header.vtable = field_map as *const ();
        (*env_ptr).slot_count = slot_count;

        let byte_len = slot_count as usize * ENV_SLOT_BYTES as usize;
        if byte_len == 0 {
            (*env_ptr).data = std::ptr::NonNull::dangling().as_ptr();
        } else {
            let layout = match Layout::array::<u8>(byte_len) {
                Ok(l) => l,
                Err(_) => {
                    list_unlink(env_ptr as *mut ObjHeader);
                    dealloc(env_ptr as *mut u8, Layout::new::<HulkEnv>());
                    return ptr::null_mut();
                }
            };
            let data_ptr = alloc_zeroed(layout);
            if data_ptr.is_null() {
                list_unlink(env_ptr as *mut ObjHeader);
                dealloc(env_ptr as *mut u8, Layout::new::<HulkEnv>());
                return ptr::null_mut();
            }
            (*env_ptr).data = data_ptr;
            ALLOC_BYTES = ALLOC_BYTES.saturating_add(byte_len);
        }
    }
    env_ptr as *mut std::ffi::c_void
}

// ─── Math builtin functions ──────────────────────────────────────────────

/// Returns the square root of a floating-point number.
#[no_mangle]
pub extern "C" fn hulk_rt_sqrt(x: f64) -> f64 {
    x.sqrt()
}

/// Returns the sine of a floating-point number (in radians).
#[no_mangle]
pub extern "C" fn hulk_rt_sin(x: f64) -> f64 {
    x.sin()
}

/// Returns the cosine of a floating-point number (in radians).
#[no_mangle]
pub extern "C" fn hulk_rt_cos(x: f64) -> f64 {
    x.cos()
}

/// Returns the exponential of a floating-point number (e^x).
#[no_mangle]
pub extern "C" fn hulk_rt_exp(x: f64) -> f64 {
    x.exp()
}

/// Returns the logarithm of a floating-point number with the specified base.
#[no_mangle]
pub extern "C" fn hulk_rt_log(base: f64, x: f64) -> f64 {
    x.log(base)
}

// ─── Random Number Generator ─────────────────────────────────────────────────

/// A simple thread-safe pseudo-random number generator (PRNG) using an atomic state.
static RNG_STATE: AtomicU64 = AtomicU64::new(0);

/// Initializes the RNG state with a seed based on the current system time.
fn init_rng() {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    RNG_STATE.store(seed ^ 0x9e3779b97f4a7c15, Ordering::Relaxed);
}

/// Returns the next 64-bit random number.
fn next_u64() -> u64 {
    let mut x = RNG_STATE.load(Ordering::Relaxed);
    if x == 0 {
        init_rng();
        x = RNG_STATE.load(Ordering::Relaxed);
    }
    x ^= x << 7;
    x ^= x >> 9;
    RNG_STATE.store(x, Ordering::Relaxed);
    x
}

/// Returns a random floating-point number in the range [0, 1).
#[no_mangle]
pub extern "C" fn hulk_rt_rand() -> f64 {
    let bits = next_u64() >> 11;
    (bits as f64) / (1u64 << 53) as f64
}

// ─── Downcast and match traps ──────────────────────────────────────────

/// Checks whether `obj` is an instance of the type identified by `target_vtable`.
///
/// Walks the ancestor chain by following vtable slot 1 (the parent vtable
/// pointer) at each step until a match is found or the chain is exhausted
/// (null parent = root type, no match).
///
/// vtable layout (canonical):
///   [0] = GC field map ptr  — skip
///   [1] = parent vtable ptr — follow for ancestor walk
///   [2..] = method ptrs
#[no_mangle]
pub extern "C" fn hulk_rt_downcast_check(
    obj: *mut std::ffi::c_void,
    target_vtable: *const (),
) -> bool {
    if obj.is_null() || target_vtable.is_null() {
        return false;
    }
    unsafe {
        let header = obj as *mut ObjHeader;
        let mut current_vtable = (*header).vtable;
        while !current_vtable.is_null() {
            if current_vtable == target_vtable {
                return true;
            }
            // Read the parent pointer (vtable[1])
            let parent_slot = (current_vtable as *const *const ()).offset(1);
            current_vtable = *parent_slot as *const ();
        }
        false
    }
}

/// Called when a downcast fails; prints an error message and aborts the program.
#[no_mangle]
pub extern "C" fn hulk_rt_downcast_fail() -> ! {
    eprintln!("runtime error: downcast failed");
    std::process::abort();
}

/// Called when the generated program hits an internal runtime inconsistency; prints an error message and aborts.
#[no_mangle]
pub extern "C" fn hulk_rt_internal_error() -> ! {
    eprintln!("runtime error: internal error");
    std::process::abort();
}

/// Called when a non-exhaustive match is encountered; prints an error message and aborts the program.
#[no_mangle]
pub extern "C" fn hulk_rt_match_fail() -> ! {
    eprintln!("runtime error: non-exhaustive match");
    std::process::abort();
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::io::FromRawFd;
    use serial_test::serial;

    /// Helper: Converts a pointer to a `HulkString` into a Rust `String`.
    unsafe fn string_from_ptr(ptr: *mut std::ffi::c_void) -> String {
        if ptr.is_null() {
            return String::new();
        }
        let s = ptr as *mut HulkString;
        let len = (*s).len as usize;
        let data = std::slice::from_raw_parts((*s).data, len);
        String::from_utf8_lossy(data).to_string()
    }

    /// Captures stdout while executing the given closure and returns the output as a String.
    fn capture_stdout<F>(f: F) -> String
    where
        F: FnOnce(),
    {
        unsafe {
            let mut pipe_fds = [0; 2];
            assert_eq!(libc::pipe(pipe_fds.as_mut_ptr()), 0, "pipe creation failed");

            let stdout_fd = libc::dup(1);
            assert!(stdout_fd >= 0, "dup failed");

            // Redirect stdout to the write end of the pipe.
            assert_eq!(libc::dup2(pipe_fds[1], 1), 1, "dup2 failed");

            // Close the write end in the parent (the child's writes go through the duplicate fd)
            libc::close(pipe_fds[1]);

            // Execute the closure.
            f();

            // Ensure the output is flushed.
            let _ = std::io::stdout().flush();
            libc::fflush(ptr::null_mut());

            // Restore stdout.
            libc::dup2(stdout_fd, 1);
            libc::close(stdout_fd);

            // Read from the read end of the pipe.
            let mut buffer = Vec::new();
            let mut file = std::fs::File::from_raw_fd(pipe_fds[0]);
            file.read_to_end(&mut buffer)
                .expect("reading from pipe failed");

            String::from_utf8_lossy(&buffer).to_string()
        }
    }

    /// Smoke test that verifies that calling hulk_rt_noop does not cause a panic or crash.
    #[test]
    #[serial(global)]
    fn noop_does_not_panic() {
        hulk_rt_noop();
    }

    /// Smoke test that verifies that calling hulk_rt_retain and hulk_rt_release
    /// on a valid pointer does not cause an immediate runtime failure.
    /// Minimal sanity check for the memory management infrastructure.
    #[test]
    #[serial(global)]
    fn retain_release_does_not_crash() {
        // Allocate exactly the size of a real built-in type and tag it
        // accordingly, so hulk_rt_release's type-tag-driven dealloc uses the
        // same Layout that was used to allocate it.
        let size = std::mem::size_of::<HulkBox>() as i64;
        let ptr = hulk_rt_alloc(size);
        assert!(!ptr.is_null());
        unsafe { (*(ptr as *mut ObjHeader)).type_tag = TAG_BOX; }
        hulk_rt_retain(ptr);   // ref_count: 0 -> 1
        hulk_rt_release(ptr);  // ref_count: 1 -> 0
    }

    // ─── String tests ────────────────────────────────────────────────────────────────

    /// Tests that converting a number to a string produces the expected result.
    #[test]
    #[serial(global)]
    fn number_to_string() {
        let ptr = hulk_rt_number_to_string(42.0);
        assert!(!ptr.is_null());
        unsafe {
            assert_eq!(string_from_ptr(ptr), "42");
            hulk_rt_release(ptr);
        }
    }

    /// Tests that converting a boolean to a string produces the expected result.
    #[test]
    #[serial(global)]
    fn bool_to_string() {
        let ptr = hulk_rt_bool_to_string(true);
        assert!(!ptr.is_null());
        unsafe {
            assert_eq!(string_from_ptr(ptr), "true");
            hulk_rt_release(ptr);
        }
        let ptr2 = hulk_rt_bool_to_string(false);
        assert!(!ptr2.is_null());
        unsafe {
            assert_eq!(string_from_ptr(ptr2), "false");
            hulk_rt_release(ptr2);
        }
    }

    /// Tests that concatenating two strings produces the expected result.
    #[test]
    #[serial(global)]
    fn string_concat() {
        let a = hulk_rt_number_to_string(10.0);
        let b = hulk_rt_bool_to_string(true);
        let c = hulk_rt_string_concat(a, b);
        assert!(!c.is_null());
        unsafe {
            assert_eq!(string_from_ptr(c), "10true");
            hulk_rt_release(a);
            hulk_rt_release(b);
            hulk_rt_release(c);
        }
    }

    /// Tests that concatenating two strings with a space produces the expected result.
    #[test]
    #[serial(global)]
    fn string_concat_space() {
        let a = hulk_rt_number_to_string(10.0);
        let b = hulk_rt_bool_to_string(true);
        let c = hulk_rt_string_concat_space(a, b);
        assert!(!c.is_null());
        unsafe {
            assert_eq!(string_from_ptr(c), "10 true");
            hulk_rt_release(a);
            hulk_rt_release(b);
            hulk_rt_release(c);
        }
    }

    // ─── Vector tests ────────────────────────────────────────────────────────────────

    /// Tests that creating a vector, setting values, and retrieving them works as expected.
    #[test]
    #[serial(global)]
    fn vector_new_and_get_set() {
        let vec = hulk_rt_vector_new(3);
        assert!(!vec.is_null());
        unsafe {
            // Set values
            let v1 = hulk_rt_number_to_string(1.0);
            let v2 = hulk_rt_number_to_string(2.0);
            let v3 = hulk_rt_number_to_string(3.0);
            hulk_rt_vector_set(vec, 0, v1);
            hulk_rt_vector_set(vec, 1, v2);
            hulk_rt_vector_set(vec, 2, v3);
            // Get and verify
            let r1 = hulk_rt_vector_get(vec, 0);
            let r2 = hulk_rt_vector_get(vec, 1);
            let r3 = hulk_rt_vector_get(vec, 2);
            assert_eq!(string_from_ptr(r1), "1");
            assert_eq!(string_from_ptr(r2), "2");
            assert_eq!(string_from_ptr(r3), "3");
            // Release all
            hulk_rt_release(v1);
            hulk_rt_release(v2);
            hulk_rt_release(v3);
            hulk_rt_release(vec as *mut std::ffi::c_void);
        }
    }

    /// Tests that iterating over a vector works as expected.
    #[test]
    #[serial(global)]
    fn vector_iterator() {
        let vec = hulk_rt_vector_new(3);
        assert!(!vec.is_null());
        unsafe {
            // Fill
            let v1 = hulk_rt_number_to_string(1.0);
            let v2 = hulk_rt_number_to_string(2.0);
            let v3 = hulk_rt_number_to_string(3.0);
            hulk_rt_vector_set(vec, 0, v1);
            hulk_rt_vector_set(vec, 1, v2);
            hulk_rt_vector_set(vec, 2, v3);
            // Iterate
            let mut count = 0;
            let mut results = Vec::new();
            while hulk_rt_vector_next(vec) {
                let cur = hulk_rt_vector_current(vec);
                results.push(string_from_ptr(cur));
                count += 1;
            }
            assert_eq!(count, 3);
            assert_eq!(results, vec!["1", "2", "3"]);
            hulk_rt_release(v1);
            hulk_rt_release(v2);
            hulk_rt_release(v3);
            hulk_rt_release(vec as *mut std::ffi::c_void);
        }
    }

    // ─── Range tests ────────────────────────────────────────────────────────────────

    /// Tests that creating a range and iterating over its values works as expected.
    #[test]
    #[serial(global)]
    fn range_basic() {
        let rng = hulk_rt_range_new(1.0, 5.0);
        assert!(!rng.is_null());
        let mut values = Vec::new();
        // SAFETY: rng is non-null (checked above) and points to a valid HulkRange.
        unsafe {
            while hulk_rt_range_next(rng) {
                values.push(hulk_rt_range_current(rng));
            }
        }
        assert_eq!(values, vec![1.0, 2.0, 3.0, 4.0]);
        hulk_rt_release(rng as *mut std::ffi::c_void);
    }

    /// Tests that creating an empty range works as expected.
    #[test]
    #[serial(global)]
    fn range_empty() {
        let rng = hulk_rt_range_new(5.0, 5.0);
        assert!(!rng.is_null());
        // SAFETY: rng is non-null (checked above) and points to a valid HulkRange.
        assert!(unsafe { !hulk_rt_range_next(rng) });
        hulk_rt_release(rng as *mut std::ffi::c_void);
    }

    // ─── Math tests ────────────────────────────────────────────────────────────

    /// Tests that the math functions work as expected.
    #[test]
    #[serial(global)]
    fn math_functions() {
        assert_eq!(hulk_rt_sqrt(4.0), 2.0);
        assert!((hulk_rt_sin(0.0) - 0.0).abs() < 1e-10);
        assert!((hulk_rt_cos(0.0) - 1.0).abs() < 1e-10);
        assert!((hulk_rt_exp(1.0) - std::f64::consts::E).abs() < 1e-10);
        assert!((hulk_rt_log(2.0, 8.0) - 3.0).abs() < 1e-10);
    }

    /// Tests that the random number generator produces values in the expected range.
    #[test]
    #[serial(global)]
    fn rand_returns_in_range() {
        let r = hulk_rt_rand();
        assert!((0.0..1.0).contains(&r));
    }

    // ─── Print tests ──────────────────────────────────────────────────

    /// Tests that printing a number outputs the expected string representation.
    #[test]
    #[serial(global)]
    fn print_outputs_number() {
        let s = hulk_rt_number_to_string(42.0);
        let output = capture_stdout(|| {
            let _ = hulk_rt_print(s);
        });
        assert_eq!(output, "42\n");
        hulk_rt_release(s);
    }

    /// Tests that printing a boolean outputs the expected string representation.
    #[test]
    #[serial(global)]
    fn print_outputs_boolean() {
        let s = hulk_rt_bool_to_string(true);
        let output = capture_stdout(|| {
            let _ = hulk_rt_print(s);
        });
        assert_eq!(output, "true\n");
        hulk_rt_release(s);
    }

    /// Tests that printing a string outputs the expected string representation.
    #[test]
    #[serial(global)]
    fn print_outputs_string_directly() {
        let s = hulk_rt_number_to_string(123.45);
        let output = capture_stdout(|| {
            let _ = hulk_rt_print(s);
        });
        let is_ok = output == "123.45\n";
        hulk_rt_release(s);
        assert!(is_ok, "output was: {:?}", output);
    }

    /// Tests that printing outputs the expected string representation.
    #[test]
    #[serial(global)]
    fn print_returns_its_argument() {
        let s = hulk_rt_number_to_string(99.0);
        let mut result: *mut std::ffi::c_void = ptr::null_mut();
        let output = capture_stdout(|| {
            result = hulk_rt_print(s);
        });
        assert_eq!(result, s);
        assert_eq!(output, "99\n");
        hulk_rt_release(s);
    }

    /// Tests that printing handles null pointers.
    #[test]
    #[serial(global)]
    fn print_handles_null() {
        let result = hulk_rt_print(ptr::null_mut());
        assert!(result.is_null());
    }

    /// Tests dynamic vector append and conversion to a fixed-size vector.
    #[test]
    #[serial(global)]
    fn dynamic_vector_append_and_to_vector() {
        let dyn_vec = hulk_rt_dynamic_vector_new();
        assert!(!dyn_vec.is_null());
        unsafe {
            let v1 = hulk_rt_number_to_string(1.0);
            let v2 = hulk_rt_number_to_string(2.0);
            let v3 = hulk_rt_number_to_string(3.0);
            hulk_rt_dynamic_vector_append(dyn_vec, v1);
            hulk_rt_dynamic_vector_append(dyn_vec, v2);
            hulk_rt_dynamic_vector_append(dyn_vec, v3);
            let fixed = hulk_rt_dynamic_vector_to_vector(dyn_vec);
            assert!(!fixed.is_null());
            // Check fixed vector length.
            assert_eq!((*fixed).len, 3);
            // Check elements.
            let e1 = hulk_rt_vector_get(fixed, 0);
            let e2 = hulk_rt_vector_get(fixed, 1);
            let e3 = hulk_rt_vector_get(fixed, 2);
            assert_eq!(string_from_ptr(e1), "1");
            assert_eq!(string_from_ptr(e2), "2");
            assert_eq!(string_from_ptr(e3), "3");
            // Clean up.
            hulk_rt_release(fixed as *mut std::ffi::c_void);
            hulk_rt_release(v1);
            hulk_rt_release(v2);
            hulk_rt_release(v3);
        }
    }
}

#[cfg(test)]
mod gc_tests {
    use super::*;
    use serial_test::serial;

    // ── Helper: manually wire a minimal TAG_OBJECT with a vtable ──────

    /// Allocates a raw object of `size` bytes, sets its header fields, and
    /// links it into the allocation list via hulk_rt_alloc.
    /// Returns the pointer cast to `*mut ObjHeader`.
    unsafe fn _make_object(size: usize) -> *mut ObjHeader {
        let ptr = hulk_rt_alloc(size as i64) as *mut ObjHeader;
        assert!(!ptr.is_null());
        (*ptr).ref_count = 1;
        (*ptr).type_tag = TAG_OBJECT;
        ptr
    }

    /// Resets global GC state between tests. Tests must call this at the
    /// start to avoid cross-test contamination from the static globals.
    unsafe fn reset_gc_state() {
        // Free everything in the allocation list without GC logic.
        let mut cur = ALLOC_LIST_HEAD;
        while !cur.is_null() {
            let next = (*cur).next;
            // Unlink before freeing to avoid stale pointers in the list.
            list_unlink(cur);
            gc_free_object(cur);
            cur = next;
        }
        ALLOC_LIST_HEAD = ptr::null_mut();
        ALLOC_BYTES = 0;
        SHADOW_TOP = 0;
    }

    // ── Test 1: shadow stack push/pop balance ─────────────────────────

    #[test]
    #[serial(global)]
    fn shadow_push_pop_balanced() {
        unsafe {
            reset_gc_state();
            let mut slot: *mut std::ffi::c_void = ptr::null_mut();
            let depth_before = SHADOW_TOP;
            hulk_rt_shadow_push(&mut slot as *mut _ as *mut std::ffi::c_void);
            let top = SHADOW_TOP;
            assert_eq!(top, depth_before + 1);
            hulk_rt_shadow_pop();
            let top = SHADOW_TOP;
            assert_eq!(top, depth_before);
        }
    }

    // ── Test 2: allocation list is maintained correctly ───────────────

    #[test]
    #[serial(global)]
    fn alloc_links_into_list() {
        unsafe {
            reset_gc_state();
            let p = hulk_rt_alloc(40) as *mut ObjHeader;
            assert!(!p.is_null());
            // The object must be the list head (freshly reset state).
            let head = ALLOC_LIST_HEAD;
            assert_eq!(head, p);
            // Clean up without GC.
            list_unlink(p);
            dealloc(p as *mut u8, Layout::from_size_align(40, 8).unwrap());
            ALLOC_BYTES = 0;
        }
    }

    // ── Test 3: mark phase reaches objects through shadow stack ───────

    #[test]
    #[serial(global)]
    fn mark_phase_marks_roots() {
        unsafe {
            reset_gc_state();

            // Allocate a string and register it as a shadow-stack root.
            let s = hulk_rt_number_to_string(42.0) as *mut ObjHeader;
            assert!(!s.is_null());
            (*s).gc_mark = 0; // ensure it starts unmarked

            let mut slot = s as *mut std::ffi::c_void;
            hulk_rt_shadow_push(&mut slot as *mut _ as *mut std::ffi::c_void);

            gc_mark_roots();

            assert_eq!((*s).gc_mark, 1, "root object should be marked");

            hulk_rt_shadow_pop();
            // Clean up.
            (*s).gc_mark = 0;
            hulk_rt_release(s as *mut std::ffi::c_void);
        }
    }

    // ── Test 4: sweep frees unmarked objects ──────────────────────────

    #[test]
    #[serial(global)]
    fn sweep_frees_unmarked() {
        unsafe {
            reset_gc_state();

            // Allocate a string but do NOT register it as a root and do NOT
            // mark it. It should be collected by the sweep.
            let s = hulk_rt_number_to_string(99.0) as *mut ObjHeader;
            assert!(!s.is_null());
            // Ensure it is in the alloc list (hulk_rt_string_from_bytes links it).
            assert!(!ALLOC_LIST_HEAD.is_null());

            // Mark phase: no roots registered → nothing gets marked.
            gc_mark_roots(); // SHADOW_TOP == 0 → no-op

            let bytes_before = ALLOC_BYTES;
            gc_sweep(); // should free `s`

            // Bytes freed must be non-zero (the string was in the list).
            assert!(
                ALLOC_BYTES < bytes_before,
                "sweep should have reduced ALLOC_BYTES"
            );
            // The list should now be empty.
            assert!(ALLOC_LIST_HEAD.is_null(), "alloc list should be empty after sweep");
        }
    }

    // ── Test 5: full collect on a simple two-node cycle ───────────────

    /// Builds a two-node cycle (A.next = B, B.next = A) with no external
    /// roots and verifies that a GC cycle reclaims both nodes.
    ///
    /// Because TAG_OBJECT requires a valid vtable with a field map to trace
    /// children, this test uses TAG_VECTOR (whose children are all traced by
    /// the mark phase unconditionally) to represent the cycle nodes.
    #[test]
    #[serial(global)]
    fn gc_collects_two_node_cycle() {
        unsafe {
            reset_gc_state();

            // Allocate two single-element vectors.
            let a = hulk_rt_vector_new(1);
            let b = hulk_rt_vector_new(1);
            assert!(!a.is_null() && !b.is_null());

            // Wire the cycle: a[0] = b, b[0] = a
            hulk_rt_retain(b as *mut std::ffi::c_void);
            *(*a).data = b as *mut std::ffi::c_void;
            hulk_rt_retain(a as *mut std::ffi::c_void);
            *(*b).data = a as *mut std::ffi::c_void;

            // Drop external references (ref counts drop to 1 each, held by the cycle)
            (*a).header.ref_count -= 1; // would normally be done by codegen release
            (*b).header.ref_count -= 1;

            // No shadow-stack roots registered for a or b.
            let bytes_before = ALLOC_BYTES;
            hulk_rt_gc_collect();
            let bytes_after = ALLOC_BYTES;

            assert!(
                bytes_after < bytes_before,
                "GC should have collected the cycle (bytes_before={}, after={})",
                bytes_before, bytes_after
            );
            assert!(ALLOC_LIST_HEAD.is_null(), "alloc list should be empty");
        }
    }
}