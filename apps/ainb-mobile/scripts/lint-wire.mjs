// C-M1-3: the app parses no wire JSON and declares no wire type. Wire shapes
// live only under src/wire/ (the ubrn bindings once they land, the stand-in
// records until then). Fails the check on the first offender.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const root = new URL("..", import.meta.url).pathname;
const offenders = [];
const rules = [
  { re: /JSON\.parse\(/, why: "JSON.parse of a wire payload; the crate decodes" },
  { re: /\b(op_id|attention_id|session_key|host_id|answered_by|floor_gen|stream_id)\b/, why: "snake_case wire field; use the decoded record" },
];

function walk(dir) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (name === "node_modules" || name.startsWith(".")) continue;
    if (statSync(p).isDirectory()) walk(p);
    else if (/\.(ts|tsx)$/.test(name)) check(p);
  }
}

function check(file) {
  const rel = relative(root, file);
  if (rel.startsWith("src/wire/") || rel.startsWith("scripts/")) return;
  const lines = readFileSync(file, "utf8").split("\n");
  lines.forEach((line, i) => {
    for (const { re, why } of rules) if (re.test(line)) offenders.push(`${rel}:${i + 1}: ${why}`);
  });
}

walk(root);
if (offenders.length) {
  console.error(offenders.join("\n"));
  process.exit(1);
}
console.log("lint-wire: clean");
