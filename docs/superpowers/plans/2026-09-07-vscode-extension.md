# HULK VS Code Extension Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the VS Code extension in `editors/vscode/`: syntax highlighting for `.hulk` files, a language client that spawns `hulk-lsp` and gets diagnostics/hover/go-to-definition/completion for free (the server side is already complete — Plans 2–5), and a `hulk.runFile` command that compiles and runs the active file via `hulk-cli`.

**Architecture:** A standard TypeScript VS Code extension. `syntaxes/hulk.tmLanguage.json` (a static TextMate grammar) and `language-configuration.json` (brackets/comments) need no server involvement. `src/extension.ts` is the only code: `activate()` starts a `vscode-languageclient` `LanguageClient` pointed at the `hulk-lsp` binary (found at `target/debug/hulk-lsp(.exe)` relative to the repo root by default, overridable via the `hulk.serverPath` setting) and registers `hulk.runFile`, which saves the active document and sends `hulk-cli <file> && <output>` to an integrated terminal — mirroring exactly what the design spec's "Running code from the editor" section describes, and reusing `hulk-cli`'s existing stderr error format and exit code as-is.

**Tech Stack:** TypeScript, `vscode-languageclient` (client side of the LSP protocol this project's server already speaks), the `vscode` extension API. Node.js and npm are already installed on this machine.

**Spec:** [docs/superpowers/specs/2026-09-01-hulk-ide-design.md](../specs/2026-09-01-hulk-ide-design.md) — "VS Code extension" and "Running code from the editor" sections.

## Global Constraints

- No packaging/distribution work (no `.vsix`, no marketplace metadata beyond the minimum `package.json` needs) — per the spec, this is a personal-use extension run from source via VS Code's "Run Extension" launch configuration or `code --extensionDevelopmentPath`.
- **`hulk.runFile` cannot be verified end-to-end in this environment.** `hulk-codegen`/`hulk-cli` require LLVM 17, which isn't installed here (a pre-existing, already-documented gap — see Plan 1's spec notes and every prior plan's "Post-plan check"). The command is written carefully and matches the spec's documented design, but whether the produced executable is `output` or `output.exe` on Windows, and whether the `cc`-based linking step works at all here, remains unverified until `hulk-cli` can actually be built on a machine with LLVM 17.
- Everything else in this plan (grammar, language configuration, the language client, diagnostics/hover/definition/completion wiring) has no such gap and gets real, automated verification.

---

### Task 1: Project scaffold

**Files:**
- Create: `editors/vscode/package.json`
- Create: `editors/vscode/tsconfig.json`
- Create: `editors/vscode/language-configuration.json`
- Create: `editors/vscode/.gitignore`

**Interfaces:**
- Produces: an npm project at `editors/vscode/` that `npm install` and `npm run compile` (added in Task 3, once there's a `src/extension.ts` to compile) can act on.

- [ ] **Step 1: Create the manifest**

Create `editors/vscode/package.json`:

```json
{
  "name": "hulk-language",
  "displayName": "HULK Language",
  "description": "Syntax highlighting, diagnostics, hover, completion, go-to-definition, and running .hulk files.",
  "version": "0.1.0",
  "publisher": "jccarmenate",
  "private": true,
  "engines": {
    "vscode": "^1.85.0"
  },
  "categories": ["Programming Languages"],
  "activationEvents": ["onLanguage:hulk"],
  "main": "./out/extension.js",
  "contributes": {
    "languages": [
      {
        "id": "hulk",
        "aliases": ["HULK", "hulk"],
        "extensions": [".hulk"],
        "configuration": "./language-configuration.json"
      }
    ],
    "grammars": [
      {
        "language": "hulk",
        "scopeName": "source.hulk",
        "path": "./syntaxes/hulk.tmLanguage.json"
      }
    ],
    "commands": [
      {
        "command": "hulk.runFile",
        "title": "HULK: Run File",
        "icon": "$(play)"
      }
    ],
    "menus": {
      "editor/title": [
        {
          "command": "hulk.runFile",
          "when": "resourceLangId == hulk",
          "group": "navigation"
        }
      ]
    },
    "configuration": {
      "title": "HULK",
      "properties": {
        "hulk.serverPath": {
          "type": "string",
          "default": "",
          "description": "Path to the hulk-lsp binary. Empty uses target/debug/hulk-lsp(.exe) relative to the repository root (development default)."
        }
      }
    }
  },
  "scripts": {
    "compile": "tsc -p ./",
    "watch": "tsc -watch -p ./"
  },
  "dependencies": {
    "vscode-languageclient": "^9.0.1"
  },
  "devDependencies": {
    "@types/node": "^20.11.0",
    "@types/vscode": "^1.85.0",
    "typescript": "^5.4.0"
  }
}
```

- [ ] **Step 2: Create the TypeScript config**

Create `editors/vscode/tsconfig.json`:

```json
{
  "compilerOptions": {
    "module": "commonjs",
    "target": "ES2022",
    "outDir": "out",
    "lib": ["ES2022"],
    "sourceMap": true,
    "rootDir": "src",
    "strict": true
  },
  "exclude": ["node_modules", ".vscode-test"]
}
```

- [ ] **Step 3: Create the language configuration**

Create `editors/vscode/language-configuration.json`:

```json
{
  "comments": {
    "lineComment": "//"
  },
  "brackets": [
    ["{", "}"],
    ["[", "]"],
    ["(", ")"]
  ],
  "autoClosingPairs": [
    { "open": "{", "close": "}" },
    { "open": "[", "close": "]" },
    { "open": "(", "close": ")" },
    { "open": "\"", "close": "\"", "notIn": ["string"] }
  ],
  "surroundingPairs": [
    ["{", "}"],
    ["[", "]"],
    ["(", ")"],
    ["\"", "\""]
  ]
}
```

- [ ] **Step 4: Ignore build output and dependencies**

Create `editors/vscode/.gitignore`:

```
node_modules/
out/
*.vsix
```

- [ ] **Step 5: Install dependencies**

Run: `cd editors/vscode && npm install`
Expected: succeeds, creates `node_modules/` and `package-lock.json`.

- [ ] **Step 6: Commit**

```bash
git add editors/vscode/package.json editors/vscode/package-lock.json editors/vscode/tsconfig.json editors/vscode/language-configuration.json editors/vscode/.gitignore
git commit -m "feat(vscode): scaffold extension project"
```

---

### Task 2: TextMate grammar

**Files:**
- Create: `editors/vscode/syntaxes/hulk.tmLanguage.json`
- Create: `editors/vscode/test/grammar.test.js`

**Interfaces:**
- Produces: `source.hulk` scope grammar referenced by `package.json`'s `contributes.grammars` (Task 1).

- [ ] **Step 1: Create the grammar**

Create `editors/vscode/syntaxes/hulk.tmLanguage.json`:

```json
{
  "$schema": "https://raw.githubusercontent.com/martinring/tmlanguage/master/tmlanguage.json",
  "name": "HULK",
  "scopeName": "source.hulk",
  "patterns": [
    { "include": "#comments" },
    { "include": "#strings" },
    { "include": "#numbers" },
    { "include": "#keywords" },
    { "include": "#constants" },
    { "include": "#types" },
    { "include": "#operators" }
  ],
  "repository": {
    "comments": {
      "patterns": [
        { "name": "comment.line.double-slash.hulk", "match": "//.*$" }
      ]
    },
    "strings": {
      "name": "string.quoted.double.hulk",
      "begin": "\"",
      "end": "\"",
      "patterns": [
        { "name": "constant.character.escape.hulk", "match": "\\\\[\"\\\\nt]" }
      ]
    },
    "numbers": {
      "name": "constant.numeric.hulk",
      "match": "\\b\\d+(\\.\\d+)?\\b"
    },
    "keywords": {
      "patterns": [
        {
          "name": "keyword.control.hulk",
          "match": "\\b(if|elif|else|while|for|in|match|case)\\b"
        },
        {
          "name": "keyword.other.hulk",
          "match": "\\b(let|function|type|inherits|protocol|extends|new|is|as|def)\\b"
        },
        {
          "name": "variable.language.hulk",
          "match": "\\b(self|base)\\b"
        }
      ]
    },
    "constants": {
      "patterns": [
        { "name": "constant.language.boolean.hulk", "match": "\\b(true|false)\\b" },
        { "name": "support.constant.math.hulk", "match": "\\b(PI|E)\\b" }
      ]
    },
    "types": {
      "name": "support.type.hulk",
      "match": "\\b(Number|String|Boolean|Object)\\b"
    },
    "operators": {
      "patterns": [
        { "name": "keyword.operator.assignment.hulk", "match": ":=|=>|=" },
        { "name": "keyword.operator.comparison.hulk", "match": "==|!=|<=|>=|<|>" },
        { "name": "keyword.operator.logical.hulk", "match": "&|\\||!" },
        { "name": "keyword.operator.arithmetic.hulk", "match": "\\+|-|\\*|/|\\^|%" },
        { "name": "keyword.operator.string.hulk", "match": "@@|@" }
      ]
    }
  }
}
```

- [ ] **Step 2: Write a validity test for the grammar**

TextMate grammars are easy to get subtly wrong (an unescaped regex character, a typo in a `$ref`-style include, a missing `repository` key) with no compiler to catch it — the failure mode is silent (that pattern just never highlights). Create `editors/vscode/test/grammar.test.js`:

```js
// Validates the TextMate grammar's structure and that every `#name`
// referenced by an `include` actually exists in `repository` — the two
// mistakes most likely to silently break highlighting with no error
// anywhere else.
const assert = require('node:assert');
const path = require('node:path');
const fs = require('node:fs');

const grammarPath = path.join(__dirname, '..', 'syntaxes', 'hulk.tmLanguage.json');
const grammar = JSON.parse(fs.readFileSync(grammarPath, 'utf8'));

assert.strictEqual(grammar.scopeName, 'source.hulk', 'scopeName must be source.hulk');
assert.ok(Array.isArray(grammar.patterns) && grammar.patterns.length > 0, 'top-level patterns must be non-empty');
assert.ok(grammar.repository && typeof grammar.repository === 'object', 'repository must exist');

function collectIncludeRefs(node, refs) {
  if (Array.isArray(node)) {
    for (const item of node) collectIncludeRefs(item, refs);
    return;
  }
  if (node && typeof node === 'object') {
    if (typeof node.include === 'string' && node.include.startsWith('#')) {
      refs.push(node.include.slice(1));
    }
    for (const value of Object.values(node)) collectIncludeRefs(value, refs);
  }
}

const refs = [];
collectIncludeRefs(grammar.patterns, refs);
collectIncludeRefs(grammar.repository, refs);

for (const ref of refs) {
  assert.ok(
    Object.prototype.hasOwnProperty.call(grammar.repository, ref),
    `include "#${ref}" has no matching repository entry`
  );
}

// Every regex in the grammar must at least compile.
function collectRegexes(node, regexes) {
  if (Array.isArray(node)) {
    for (const item of node) collectRegexes(item, regexes);
    return;
  }
  if (node && typeof node === 'object') {
    for (const key of ['match', 'begin', 'end']) {
      if (typeof node[key] === 'string') regexes.push(node[key]);
    }
    for (const value of Object.values(node)) collectRegexes(value, regexes);
  }
}

const regexes = [];
collectRegexes(grammar, regexes);
for (const source of regexes) {
  assert.doesNotThrow(() => new RegExp(source), `invalid regex: ${source}`);
}

console.log(`grammar.test.js: OK (${refs.length} include refs, ${regexes.length} regexes checked)`);
```

- [ ] **Step 3: Run the test**

Run: `cd editors/vscode && node test/grammar.test.js`
Expected: prints `grammar.test.js: OK (...)` and exits 0.

- [ ] **Step 4: Commit**

```bash
git add editors/vscode/syntaxes/hulk.tmLanguage.json editors/vscode/test/grammar.test.js
git commit -m "feat(vscode): add HULK TextMate grammar with a structural validity test"
```

---

### Task 3: Language client

**Files:**
- Create: `editors/vscode/src/extension.ts`

**Interfaces:**
- Produces: `activate(context)` / `deactivate()`, the two exports VS Code's extension host requires (wired via `package.json`'s `main`, Task 1).

- [ ] **Step 1: Write the extension entry point**

Create `editors/vscode/src/extension.ts`:

```typescript
import * as path from 'path';
import * as vscode from 'vscode';
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from 'vscode-languageclient/node';

let client: LanguageClient | undefined;

export function activate(context: vscode.ExtensionContext): void {
  const serverPath = resolveServerPath(context);

  const serverOptions: ServerOptions = {
    command: serverPath,
    args: [],
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: 'file', language: 'hulk' }],
  };

  client = new LanguageClient(
    'hulk',
    'HULK Language Server',
    serverOptions,
    clientOptions
  );
  context.subscriptions.push({ dispose: () => client?.stop() });
  client.start();
}

export function deactivate(): Thenable<void> | undefined {
  return client?.stop();
}

/// Resolves the `hulk-lsp` binary path: the `hulk.serverPath` setting if
/// set, otherwise the development build at `target/debug/hulk-lsp(.exe)`
/// relative to the repository root (this extension lives at
/// `editors/vscode/`, so the repository root is two levels up).
function resolveServerPath(context: vscode.ExtensionContext): string {
  const configured = vscode.workspace
    .getConfiguration('hulk')
    .get<string>('serverPath');
  if (configured) {
    return configured;
  }
  const exeName = process.platform === 'win32' ? 'hulk-lsp.exe' : 'hulk-lsp';
  return context.asAbsolutePath(path.join('..', '..', 'target', 'debug', exeName));
}
```

- [ ] **Step 2: Compile**

Run: `cd editors/vscode && npm run compile`
Expected: succeeds with no TypeScript errors, produces `out/extension.js`.

- [ ] **Step 3: Commit**

```bash
git add editors/vscode/src/extension.ts
git commit -m "feat(vscode): wire the language client to hulk-lsp"
```

---

### Task 4: `hulk.runFile` command

**Files:**
- Modify: `editors/vscode/src/extension.ts`

**Interfaces:**
- Produces: the `hulk.runFile` command registered in Task 3's `activate()`, matching the `contributes.commands` entry from Task 1.

- [ ] **Step 1: Add the command implementation**

In `editors/vscode/src/extension.ts`, add this after `resolveServerPath`:

```typescript
let hulkTerminal: vscode.Terminal | undefined;

function getOrCreateHulkTerminal(): vscode.Terminal {
  if (!hulkTerminal || hulkTerminal.exitStatus !== undefined) {
    hulkTerminal = vscode.window.createTerminal('HULK');
  }
  return hulkTerminal;
}

/// Compiles and runs the active `.hulk` file via `hulk-cli`, in the
/// integrated terminal. `hulk-cli` already writes compiler errors to
/// stderr in `(line,col) TYPE: message` form and exits non-zero on
/// failure, so `&&` naturally skips execution when compilation fails.
async function runActiveFile(context: vscode.ExtensionContext): Promise<void> {
  const editor = vscode.window.activeTextEditor;
  if (!editor || editor.document.languageId !== 'hulk') {
    vscode.window.showErrorMessage('Open a .hulk file to run it.');
    return;
  }

  await editor.document.save();

  const filePath = editor.document.fileName;
  const dir = path.dirname(filePath);
  const cliName = process.platform === 'win32' ? 'hulk-cli.exe' : 'hulk-cli';
  const cliPath = context.asAbsolutePath(
    path.join('..', '..', 'target', 'debug', cliName)
  );

  const terminal = getOrCreateHulkTerminal();
  terminal.show();
  terminal.sendText(`cd "${dir}"`);
  if (process.platform === 'win32') {
    // PowerShell (VS Code's default Windows shell): `hulk-cli` writes
    // `./output` per its grader contract, but whether that lands as
    // `output` or `output.exe` on Windows hasn't been verified in this
    // environment (no LLVM 17 to build hulk-cli with) — try both.
    terminal.sendText(
      `& "${cliPath}" "${filePath}" && (if (Test-Path .\\output.exe) { .\\output.exe } else { .\\output })`
    );
  } else {
    terminal.sendText(`"${cliPath}" "${filePath}" && ./output`);
  }
}
```

Then register the command inside `activate()` — add this line right before the closing brace of `activate`:

```typescript
  context.subscriptions.push(
    vscode.commands.registerCommand('hulk.runFile', () => runActiveFile(context))
  );
```

- [ ] **Step 2: Compile**

Run: `cd editors/vscode && npm run compile`
Expected: succeeds with no TypeScript errors.

- [ ] **Step 3: Commit**

```bash
git add editors/vscode/src/extension.ts
git commit -m "feat(vscode): add hulk.runFile command"
```

---

### Task 5: Launch verification

**Files:** none (verification only).

- [ ] **Step 1: Build the LSP server the extension will launch**

Run: `cargo build -p hulk-lsp` (from the repository root)
Expected: builds successfully — this plan's default `hulk.serverPath` points at this exact binary.

- [ ] **Step 2: Create a sample workspace**

Create a throwaway folder outside the repo (e.g. in the scratchpad directory) containing one file, `sample.hulk`:

```
type Greeter {
    name: String = "world";
    greet(): String => "Hello, " @ self.name @ "!";
}

let g = new Greeter() in
print(g.greet());
```

- [ ] **Step 3: Launch VS Code with the extension loaded**

Run (from `editors/vscode`): `code --extensionDevelopmentPath="$(pwd)" --new-window --wait=false "<path to the sample workspace folder>"`

Expected: a new VS Code window opens with `sample.hulk` visible in the file explorer. This is the same mechanism VS Code's own "Run Extension" (F5) launch configuration uses — running it from the command line here just makes it scriptable.

- [ ] **Step 4: Verify — ask before using computer-use**

Everything up to this point (compilation, the grammar test, the running server) is verified without touching the user's screen. Confirming the extension actually *renders* correctly — syntax highlighting colors, a hover tooltip appearing, diagnostics squiggles — needs to be seen. Ask the user for permission before using computer-use to open `sample.hulk` in the launched window and take a screenshot; if they'd rather verify it themselves, tell them what to check:
  1. `sample.hulk` has syntax highlighting (keywords, strings, types colored).
  2. Hovering over `self.name` shows its type in a tooltip.
  3. Ctrl-clicking (or F12 on) `g` in `g.greet()` jumps to `let g = ...`.
  4. Typing `g.` after the last line offers `greet` as a completion.
  5. The bottom-right corner of the editor title bar (or the command palette) shows a "HULK: Run File" entry — but per this plan's "Global Constraints", running it is not expected to work yet without LLVM 17.

- [ ] **Step 5: Close the verification window and clean up**

Close the VS Code window opened in Step 3. Delete the throwaway sample workspace folder if it was created outside the repo.

---

## Post-plan check

- [ ] Re-read `package.json`'s `contributes` section against `syntaxes/hulk.tmLanguage.json` and `language-configuration.json` — confirm the `scopeName` and file paths referenced actually match what Tasks 1–2 created.
- [ ] Confirm `editors/vscode/node_modules/` and `editors/vscode/out/` are excluded from git (`git status` after Task 1 should not show them).
- [ ] Confirm the spec's remaining open item ("Exact terminal invocation for `hulk.runFile`... Windows `.exe` handling") is now either resolved or explicitly still open — per this plan, it's the latter, documented in "Global Constraints" and Step 1 of `runActiveFile`'s implementation.
