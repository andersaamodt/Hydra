import { spawn } from "node:child_process";

const runtime = process.argv[2];
if (!runtime) throw new Error("runtime path required");

function request(message) {
  return new Promise((resolve, reject) => {
    const child = spawn(runtime, ["native-host"], { stdio: ["pipe", "pipe", "inherit"] });
    const encoded = Buffer.from(JSON.stringify(message));
    const length = Buffer.alloc(4);
    length.writeUInt32LE(encoded.length);
    child.stdin.end(Buffer.concat([length, encoded]));
    const chunks = [];
    child.stdout.on("data", chunk => chunks.push(chunk));
    child.on("error", reject);
    child.on("close", code => {
      if (code !== 0) return reject(new Error(`native host exited ${code}`));
      const response = Buffer.concat(chunks);
      if (response.length < 4) return reject(new Error("native response was not framed"));
      const size = response.readUInt32LE(0);
      resolve(JSON.parse(response.subarray(4, 4 + size).toString("utf8")));
    });
  });
}

function rawRequest(bytes) {
  return new Promise((resolve, reject) => {
    const child = spawn(runtime, ["native-host"], { stdio: ["pipe", "pipe", "pipe"] });
    const stderr = [];
    child.stderr.on("data", chunk => stderr.push(chunk));
    child.on("error", reject);
    child.on("close", code => resolve({
      code,
      error: Buffer.concat(stderr).toString("utf8"),
    }));
    child.stdin.end(bytes);
  });
}

(async () => {
  const ping = await request({
    protocol: "hydra-native-bridge/v1",
    kind: "ping",
    redditUrl: ""
  });
  if (!ping.ok || ping.protocol !== "hydra-native-bridge/v1") throw new Error("ping failed");

  const unsupported = await request({
    protocol: "hydra-native-bridge/v1",
    kind: "unsupported_action",
    redditUrl: "https://www.reddit.com/r/science/comments/abc/thread/"
  });
  if (unsupported.ok || !String(unsupported.error).includes("unsupported")) {
    throw new Error(`unsupported request was not rejected: ${JSON.stringify(unsupported)}`);
  }

  const rejected = await request({
    protocol: "hydra-native-bridge/v1",
    kind: "open_reddit",
    redditUrl: "https://example.com/steal"
  });
  if (rejected.ok) throw new Error("non-Reddit URL was accepted");

  for (const url of [
    "http://www.reddit.com/r/science/",
    "https://www.reddit.com.evil.test/r/science/",
    "https://example.com/?next=https://www.reddit.com/r/science/",
    "https://attacker@www.reddit.com/r/science/",
    "https://www.reddit.com:444/r/science/",
  ]) {
    const hostile = await request({
      protocol: "hydra-native-bridge/v1",
      kind: "open_reddit",
      redditUrl: url,
    });
    if (hostile.ok) throw new Error(`hostile URL was accepted: ${url}`);
  }

  const oversizedLength = Buffer.alloc(4);
  oversizedLength.writeUInt32LE(1_048_577);
  const oversized = await rawRequest(oversizedLength);
  if (oversized.code === 0 || !oversized.error.includes("length is invalid")) {
    throw new Error("oversized native frame was not rejected before allocation");
  }

  const truncatedLength = Buffer.alloc(4);
  truncatedLength.writeUInt32LE(10);
  const truncated = await rawRequest(Buffer.concat([truncatedLength, Buffer.from("{}")]));
  if (truncated.code === 0) throw new Error("truncated native frame was accepted");
})().catch(error => {
  console.error(error);
  process.exit(1);
});
