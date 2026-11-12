// Tests the extension's grammar with vscode-textmate, the engine VS Code itself
// uses to color a file.
//
//   node grammar.js test          the cases in cases.js, then every .jde example
//   node grammar.js show <file>   each token in a file and its scopes
//
// `test` exits non-zero on any failure. vscode-ci.yml runs it.
const fs = require('fs');
const path = require('path');
const vsctm = require('vscode-textmate');
const oniguruma = require('vscode-oniguruma');

const GRAMMAR = path.join(__dirname, '../syntaxes/jade.tmLanguage.json');
const REPO = path.join(__dirname, '../..');

async function loadGrammar() {
  const wasm = fs.readFileSync(require.resolve('vscode-oniguruma/release/onig.wasm')).buffer;
  await oniguruma.loadWASM(wasm);
  const registry = new vsctm.Registry({
    onigLib: Promise.resolve({
      createOnigScanner: (s) => new oniguruma.OnigScanner(s),
      createOnigString: (s) => new oniguruma.OnigString(s),
    }),
    loadGrammar: async () => vsctm.parseRawGrammar(fs.readFileSync(GRAMMAR, 'utf8'), GRAMMAR),
  });
  return registry.loadGrammar('source.jade');
}

// Each token as { line, text, scopes }, without the root `source.jade` scope.
function tokenize(grammar, text) {
  const out = [];
  let state = vsctm.INITIAL;
  text.split('\n').forEach((line, i) => {
    const r = grammar.tokenizeLine(line, state);
    for (const t of r.tokens) {
      out.push({ line: i + 1, text: line.slice(t.startIndex, t.endIndex), scopes: t.scopes.slice(1).join(' ') });
    }
    state = r.ruleStack;
  });
  return out;
}

// Each case is [source, [[token text, scope], ...]]. The token must carry the
// scope, or with a leading `!`, every token with that text must lack it.
function runCases(grammar) {
  const cases = require('./cases.js');
  let failed = 0;
  for (const [src, expect] of cases) {
    const toks = tokenize(grammar, src);
    for (const [text, scope] of expect) {
      const negated = scope.startsWith('!');
      const bare = negated ? scope.slice(1) : scope;
      const same = toks.filter((t) => t.text.trim() === text);
      const ok = negated
        ? same.length > 0 && same.every((t) => !t.scopes.includes(bare))
        : same.some((t) => t.scopes.includes(bare));
      if (!ok) {
        const got = same.map((t) => t.scopes || '(none)').join(' | ') || 'no such token';
        console.log(`FAIL ${JSON.stringify(src.split('\n')[0])}: ${JSON.stringify(text)} wants ${scope}, got ${got}`);
        failed++;
      }
    }
  }
  console.log(`${cases.length} cases, ${failed} failures`);
  return failed;
}

// Every word the lexer reads as a keyword or word operator, from
// compiler/src/frontend/lexer.rs, plus the nil family and self.
const KEYWORDS = new Set(('let fn return if elif else while for in is break continue struct extend class ' +
  'interface prompt use raise try catch async await yield from as true false and or not nil None null self').split(' '));

function jdeFiles(dir, acc = []) {
  if (!fs.existsSync(dir)) return acc;
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) jdeFiles(p, acc);
    else if (e.name.endsWith('.jde')) acc.push(p);
  }
  return acc;
}

// The examples compile, so the grammar must mark nothing in them invalid, and
// must give every keyword outside a string or comment a scope. A file named
// *_error.jde is a fixture the compiler refuses, and may hold removed syntax.
function scanExamples(grammar) {
  const files = [
    ...jdeFiles(path.join(REPO, 'examples')),
    ...jdeFiles(path.join(REPO, 'compiler/src')),
    ...jdeFiles(path.join(__dirname, '../examples')),
  ];
  let problems = 0;
  for (const f of files) {
    const rel = path.relative(REPO, f);
    const fixture = /_error\.jde$/.test(f);
    for (const t of tokenize(grammar, fs.readFileSync(f, 'utf8'))) {
      const word = t.text.trim();
      const inText = /comment|string(?!.*meta\.embedded)/.test(t.scopes);
      if (!fixture && t.scopes.includes('invalid')) {
        console.log(`INVALID ${rel}:${t.line} ${JSON.stringify(t.text)} ${t.scopes}`);
        problems++;
      }
      if (!inText && KEYWORDS.has(word) && !/keyword|constant|invalid|storage/.test(t.scopes)) {
        console.log(`UNCOLORED KEYWORD ${rel}:${t.line} ${JSON.stringify(word)} ${t.scopes}`);
        problems++;
      }
    }
  }
  console.log(`${files.length} example files, ${problems} problems`);
  return problems;
}

(async () => {
  const [mode, file] = process.argv.slice(2);
  const grammar = await loadGrammar();
  if (mode === 'show' && file) {
    for (const t of tokenize(grammar, fs.readFileSync(file, 'utf8'))) {
      if (t.text.trim()) console.log(`${t.line}\t${JSON.stringify(t.text)}\t${t.scopes}`);
    }
  } else if (mode === 'test') {
    const bad = runCases(grammar) + scanExamples(grammar);
    process.exit(bad ? 1 : 0);
  } else {
    console.log('usage: node grammar.js test | show <file.jde>');
    process.exit(2);
  }
})();
