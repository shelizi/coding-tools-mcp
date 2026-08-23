import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

function hashContent(content) {
  return createHash("sha256").update(content).digest("hex").slice(0, 12);
}

export async function extractInlineAssets(outDir, base = "") {
  const indexPath = path.join(outDir, "index.html");
  let html = await readFile(indexPath, "utf8");
  const chunkDir = path.join(outDir, "_app", "immutable", "chunks");
  await mkdir(chunkDir, { recursive: true });
  const prefix = `${base}/_app/immutable/chunks`.replace(/\/{2,}/g, "/");
  const writes = [];

  html = html.replace(/<script([^>]*)>([\s\S]*?)<\/script>/gi, (full, attrs, body) => {
    if (/\bsrc\s*=/i.test(attrs)) return full;
    const trimmed = String(body).trim();
    if (!trimmed) return full;
    const name = `inline-${hashContent(trimmed)}.js`;
    writes.push(writeFile(path.join(chunkDir, name), trimmed, "utf8"));
    const type = /\btype\s*=\s*["']module["']/i.test(attrs) ? ' type="module"' : "";
    return `<script${type} src="${prefix}/${name}"></script>`;
  });

  html = html.replace(/<style\b([^>]*)>([\s\S]*?)<\/style>/gi, (full, _attrs, body) => {
    const trimmed = String(body).trim();
    if (!trimmed) return full;
    const name = `inline-${hashContent(trimmed)}.css`;
    writes.push(writeFile(path.join(chunkDir, name), trimmed, "utf8"));
    return `<link rel="stylesheet" href="${prefix}/${name}">`;
  });

  await Promise.all(writes);
  await writeFile(indexPath, html, "utf8");
  return html;
}

const invoked = fileURLToPath(import.meta.url) === path.resolve(process.argv[1] ?? "");
if (invoked) {
  const outDir = process.argv[2];
  const base = process.argv[3] ?? "";
  if (!outDir) {
    console.error("usage: extract-sveltekit-inline.mjs <outDir> [base]");
    process.exit(1);
  }
  await extractInlineAssets(outDir, base);
}
