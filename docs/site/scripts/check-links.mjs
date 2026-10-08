// Checks that every internal link and image of the built site, and every anchor, leads somewhere: node scripts/check-links.mjs
// after a build. The base path is BASE, as in astro.config.mjs.
import { readFileSync, readdirSync, statSync, existsSync } from 'node:fs';
import { join, dirname, posix } from 'node:path';
import { fileURLToPath } from 'node:url';
const dist = join(dirname(dirname(fileURLToPath(import.meta.url))), 'dist');
const base = process.env.BASE ?? '/mabat';
const files = [];
const walk = (d) => readdirSync(d).forEach((f) => { const p = join(d, f); statSync(p).isDirectory() ? walk(p) : p.endsWith('.html') && files.push(p); });
walk(dist);
const ids = new Map();
for (const f of files) ids.set(f, new Set([...readFileSync(f, 'utf8').matchAll(/\sid="([^"]+)"/g)].map((m) => m[1])));
let bad = 0, checked = 0;
for (const f of files) {
  const url = base + '/' + f.slice(dist.length + 1).replace(/index\.html$/, '');
  for (const [, href] of readFileSync(f, 'utf8').matchAll(/(?:href|src)="([^"]+)"/g)) {
    if (/^(https?:|mailto:|data:)/.test(href)) continue;
    const [path, hash] = href.split('#');
    const abs = path === '' ? url : posix.normalize(path.startsWith('/') ? path : posix.join(url.endsWith('/') ? url : dirname(url) + '/', path));
    if (!abs.startsWith(base)) { console.log('outside base', f, href); bad++; continue; }
    let target = join(dist, abs.slice(base.length));
    if (existsSync(target) && statSync(target).isDirectory()) target = join(target, 'index.html');
    checked++;
    if (!existsSync(target)) { console.log('missing', f.slice(dist.length), href); bad++; continue; }
    if (hash && target.endsWith('.html') && !ids.get(target)?.has(decodeURIComponent(hash))) { console.log('no anchor', f.slice(dist.length), href); bad++; }
  }
}
console.log(`${checked} links, ${bad} broken`);
if (bad > 0) process.exit(1);
