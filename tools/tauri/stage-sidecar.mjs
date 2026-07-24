import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const repository = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const cargoMetadata = JSON.parse(execFileSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], {
  cwd: repository,
  encoding: "utf8",
}));
const targetTriple = execFileSync("rustc", ["--print", "host-tuple"], {
  cwd: repository,
  encoding: "utf8",
}).trim();
const profile = process.argv[2] === "release" ? "release" : "debug";
const executableSuffix = process.platform === "win32" ? ".exe" : "";
const source = join(cargoMetadata.target_directory, profile, `hydra-runtime${executableSuffix}`);
const destinationDirectory = join(repository, "apps", "desktop", "tauri", "binaries");
const destination = join(
  destinationDirectory,
  `hydra-runtime-${targetTriple}${executableSuffix}`,
);

mkdirSync(destinationDirectory, { recursive: true });
copyFileSync(source, destination);
if (process.platform !== "win32") chmodSync(destination, 0o755);
console.log(`Staged ${destination}`);
