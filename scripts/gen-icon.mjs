// 无依赖生成应用图标：icons/icon.ico（256x256，PNG-in-ICO）+ icons/icon.png
// 图案：蓝色圆角方块上的白色"解锁挂锁"
import { deflateSync } from "node:zlib";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const iconsDir = join(root, "src-tauri", "icons");
mkdirSync(iconsDir, { recursive: true });

const SIZE = 256;
const SS = 2; // 2x 超采样抗锯齿

// ---- 简单 SDF 形状 ----
const sdRoundRect = (px, py, cx, cy, hw, hh, r) => {
  const dx = Math.abs(px - cx) - (hw - r);
  const dy = Math.abs(py - cy) - (hh - r);
  const ox = Math.max(dx, 0), oy = Math.max(dy, 0);
  return Math.hypot(ox, oy) + Math.min(Math.max(dx, dy), 0) - r;
};
const sdRingLeft = (px, py, cx, cy, rMid, halfW) => {
  // 左半圆环：半径带 [rMid-halfW, rMid+halfW]，角度 90°..270°（左侧）
  const dx = px - cx, dy = cy - py; // y 翻转，角度以屏幕坐标计
  const d = Math.abs(Math.hypot(dx, dy) - rMid) - halfW;
  const onLeft = dx <= 1 || dy > 0; // 左半 + 顶部过渡
  return onLeft ? d : Math.max(d, Math.abs(dx) - halfW);
};
const cov = (d) => Math.min(Math.max(0.5 - d * SS, 0), 1);

function render() {
  const rgba = Buffer.alloc(SIZE * SIZE * 4, 0);
  for (let y = 0; y < SIZE; y++) {
    for (let x = 0; x < SIZE; x++) {
      let bg = 0, fg = 0;
      for (let sy = 0; sy < SS; sy++) {
        for (let sx = 0; sx < SS; sx++) {
          const px = (x + (sx + 0.5) / SS) - SIZE / 2;
          const py = (y + (sy + 0.5) / SS) - SIZE / 2;
          // 背景圆角方块（垂直渐变）
          const dBg = sdRoundRect(px, py, 0, 0, 128, 128, 52);
          bg += cov(dBg);
          // 前景：解锁挂锁（白色）
          const body = sdRoundRect(px, py, 0, 42, 62, 36, 14);
          const shackle = sdRingLeft(px, py, 0, -26, 34, 9);
          fg += cov(Math.min(body, shackle));
        }
      }
      const n = SS * SS;
      bg /= n; fg /= n;
      const i = (y * SIZE + x) * 4;
      if (fg > 0) {
        // 白色前景，叠在渐变背景之上
        const t = y / SIZE;
        const br = 255, bgc = 255, bb = 255;
        const rr = Math.round(43 + (0 - 43) * t);
        const gg = Math.round(157 + (87 - 157) * t);
        const cc = Math.round(244 + (168 - 244) * t);
        const a = fg;
        rgba[i] = Math.round((br * a + rr * (1 - a)));
        rgba[i + 1] = Math.round((bgc * a + gg * (1 - a)));
        rgba[i + 2] = Math.round((bb * a + cc * (1 - a)));
        rgba[i + 3] = 255;
      } else if (bg > 0) {
        const t = y / SIZE;
        rgba[i] = Math.round(43 + (0 - 43) * t);
        rgba[i + 1] = Math.round(157 + (87 - 157) * t);
        rgba[i + 2] = Math.round(244 + (168 - 244) * t);
        rgba[i + 3] = Math.round(bg * 255);
      }
    }
  }
  return rgba;
}

// ---- PNG 编码（RGBA8，无依赖）----
const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
const crc32 = (buf) => {
  let c = 0xffffffff;
  for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};
const chunk = (type, data) => {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([len, body, crc]);
};
const encodePng = (rgba, size) => {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; ihdr[9] = 6; // 8bit RGBA
  const raw = Buffer.alloc((size * 4 + 1) * size);
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0; // filter: none
    rgba.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4);
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
};

const png = encodePng(render(), SIZE);
writeFileSync(join(iconsDir, "icon.png"), png);

// ---- ICO 容器（单条 256px PNG 条目）----
const header = Buffer.alloc(6);
header.writeUInt16LE(0, 0); // reserved
header.writeUInt16LE(1, 2); // type: icon
header.writeUInt16LE(1, 4); // count
const entry = Buffer.alloc(16);
entry[0] = 0; entry[1] = 0; // 256x256 (0 = 256)
entry[2] = 0; entry[3] = 0; // palette
entry.writeUInt16LE(1, 4); // planes
entry.writeUInt16LE(32, 6); // bpp
entry.writeUInt32LE(png.length, 8);
entry.writeUInt32LE(22, 12); // data offset
writeFileSync(join(iconsDir, "icon.ico"), Buffer.concat([header, entry, png]));

console.log("icons generated:", iconsDir);
