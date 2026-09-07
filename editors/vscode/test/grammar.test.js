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
