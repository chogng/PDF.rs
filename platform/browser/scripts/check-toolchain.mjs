import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const typescript = require("typescript");

const expectedNode = "v24.18.0";
const expectedNpm = "11.16.0";
const expectedTypeScript = "5.9.3";
const mismatches = [];

if (process.version !== expectedNode) {
  mismatches.push(`Node ${process.version}; expected ${expectedNode}`);
}
if (typescript.version !== expectedTypeScript) {
  mismatches.push(
    `TypeScript ${typescript.version}; expected ${expectedTypeScript}`,
  );
}
const npmVersion = process.env.npm_config_user_agent
  ?.split(" ")[0]
  ?.replace(/^npm\//u, "");
if (npmVersion !== undefined && npmVersion !== expectedNpm) {
  mismatches.push(`npm ${npmVersion}; expected ${expectedNpm}`);
}

if (mismatches.length !== 0) {
  throw new Error(`browser toolchain mismatch: ${mismatches.join("; ")}`);
}
