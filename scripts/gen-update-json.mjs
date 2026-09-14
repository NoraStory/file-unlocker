// 生成 update.json：计算 release 产物的 SHA256，
// 输出供 GitHub/Gitee raw 托管的更新清单（程序自动更新时拉取）
// 用法：npm run tauri build 完成后执行 node scripts/gen-update-json.mjs
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
const version = pkg.version;
if (!/^\d+\.\d+\.\d+$/.test(version)) {
  console.error("version 格式异常:", version);
  process.exit(1);
}

const tag = `v${version}`;
const base = `https://github.com/NoraStory/file-unlocker/releases/download/${tag}`;
const assets = [];

for (const [file, kind] of [
  [`FileUnlocker_${version}_x64-setup.exe`, "installer"],
  ["file-unlocker.exe", "portable"],
]) {
  const p = join(root, "src-tauri", "target", "release", "bundle", "nsis", file);
  const fallback = join(root, "src-tauri", "target", "release", file);
  const path = existsSync(p) ? p : fallback;
  if (!existsSync(path)) {
    console.error("缺少产物:", path);
    process.exit(1);
  }
  const buf = readFileSync(path);
  const sha256 = createHash("sha256").update(buf).digest("hex");
  assets.push({ name: file, kind, url: `${base}/${file}`, sha256 });
  console.log(`${file}: sha256=${sha256} (${(buf.length / 1048576).toFixed(1)} MB)`);
}

const manifest = {
  version,
  notes: `FileUnlocker ${version} 更新`,
  assets,
};

writeFileSync(join(root, "update.json"), JSON.stringify(manifest, null, 2) + "\n", "utf8");
console.log("已生成 update.json");
