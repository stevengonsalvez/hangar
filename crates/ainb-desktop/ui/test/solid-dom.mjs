// Node module hooks that compile this window's `.tsx` components for the DOM,
// so a component test can mount one into a real document (happy-dom) and hold
// on to its nodes. The same two steps as `solid-ssr.mjs`: TypeScript stripped
// with the compiler the build uses (JSX kept), then the Solid preset, here in
// its `dom` mode, the mode the window itself is built in. Used only by
// `npm run test:dom`, which runs with the `browser` condition so `solid-js`
// resolves to its client build.

import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { transformAsync } from "@babel/core";
import solid from "babel-preset-solid";
import ts from "typescript";

// xterm ships CommonJS as `main` and ES modules as `module`, which is what
// Vite bundles. Node reads only `main`, whose named exports it cannot see, so
// a test that mounts the whole window (`terminal.tsx`) resolves xterm the way
// the build does.
export async function resolve(specifier, context, nextResolve) {
  if (!specifier.startsWith("@xterm/")) return nextResolve(specifier, context);
  const resolved = await nextResolve(specifier, context);
  return { ...resolved, url: resolved.url.replace(/\.js$/, ".mjs"), format: "module" };
}

export async function load(url, context, nextLoad) {
  // A stylesheet the window imports is the bundler's to inject; a DOM test
  // reads markup and attributes, never computed styles, so it loads as empty.
  if (url.endsWith(".css")) return { format: "module", source: "", shortCircuit: true };
  if (!url.endsWith(".tsx")) return nextLoad(url, context);
  const path = fileURLToPath(url);
  const source = await readFile(path, "utf8");
  const stripped = ts.transpileModule(source, {
    fileName: path,
    compilerOptions: {
      jsx: ts.JsxEmit.Preserve,
      module: ts.ModuleKind.ESNext,
      target: ts.ScriptTarget.ES2022,
      verbatimModuleSyntax: true,
    },
  }).outputText;
  const compiled = await transformAsync(stripped, {
    filename: path.replace(/\.tsx$/, ".jsx"),
    babelrc: false,
    configFile: false,
    presets: [[solid, { generate: "dom" }]],
  });
  // Vite defines `import.meta.env` for the window; a test runs as a build does.
  const env = "import.meta.env ??= { DEV: false, PROD: true, MODE: \"test\" };\n";
  return { format: "module", source: env + compiled.code, shortCircuit: true };
}
