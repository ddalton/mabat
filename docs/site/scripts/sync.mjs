// Writes the pages of the site that come from the repository: the MPA specification, the design
// document and the changelog from their Markdown, and the reference pages from docs/mpa.json.
// The repository files stay the source of truth; the written pages are not committed.
import { copyFileSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const site = dirname(dirname(fileURLToPath(import.meta.url)));
const repo = join(site, '..', '..');
const content = join(site, 'src', 'content', 'docs');
const github = 'https://github.com/ddalton/mabat/blob/main';

const read = (path) => readFileSync(join(repo, path), 'utf8');

function write(slug, frontmatter, body) {
  const path = join(content, `${slug}.md`);
  mkdirSync(dirname(path), { recursive: true });
  const yaml = Object.entries(frontmatter)
    .map(([key, value]) => `${key}: ${JSON.stringify(value)}`)
    .join('\n');
  writeFileSync(path, `---\n${yaml}\n---\n\n${body.trim()}\n`);
}

/** The relative URL from the page at `from` to the page at `to`, both slugs. */
function relative(from, to) {
  const page = (slug) => slug.replace(/(^|\/)index$/, '');
  const depth = page(from) === '' ? 0 : page(from).split('/').length;
  return `${'../'.repeat(depth)}${page(to) === '' ? '' : `${page(to)}/`}`;
}

/** The anchor of a rule, such as `mpa-write-9`. */
const anchor = (id) => id.toLowerCase();

/** A link to a rule of the specification, from the page at `from`. */
const ruleLink = (from, id) => `[${id}](${from === 'spec/mpa' ? '' : relative(from, 'spec/mpa')}#${anchor(id)})`;

/** Repository links of a document in `dir` rewritten for the page at `from`. */
function rewriteLinks(markdown, dir, from) {
  const pages = { 'docs/mpa.md': 'spec/mpa', 'docs/design.md': 'design', 'CHANGELOG.md': 'changelog' };
  return markdown.replace(/\]\(([^)#\s]+)(#[^)\s]*)?\)/g, (match, target, hash = '') => {
    if (/^[a-z]+:/.test(target)) return match;
    const path = join(dir, target).replace(/\\/g, '/');
    if (pages[path]) return `](${relative(from, pages[path])}${hash})`;
    return `](${github}/${path}${hash})`;
  });
}

/** The document without its first heading, which becomes the page title, and without its list of contents,
 * which the table of contents of the page replaces. */
function withoutTitle(markdown) {
  return markdown.replace(/^# .*\n+/, '').replace(/^## Contents\n[\s\S]*?\n(?=## )/m, '').replace(/\n---\n+(?=## 1\.)/, '\n');
}

/** References to rules in a line linked from the page at `from`, outside inline code. */
function linkRules(line, from) {
  return line
    .split(/(`[^`]*`)/)
    .map((part) => (part.startsWith('`') ? part : part.replace(/\bMPA-[A-Z]+-\d+\b/g, (ref) => ruleLink(from, ref))))
    .join('');
}

// The specification: rule identifiers become anchors, and references to rules become links
{
  let spec = withoutTitle(read('docs/mpa.md'));
  spec = rewriteLinks(spec, 'docs', 'spec/mpa');
  spec = spec
    .split('\n')
    .map((line) => {
      const definition = line.match(/^(\s*)- \*\*(MPA-[A-Z]+-\d+)\*\* (.*)$/);
      if (definition) {
        const [, indent, id, text] = definition;
        const linked = linkRules(text, 'spec/mpa');
        return `${indent}- <span id="${anchor(id)}" class="rule-id"></span>**${id}** ${linked}`;
      }
      return line.startsWith('```') ? line : linkRules(line, 'spec/mpa');
    })
    .join('\n');
  write(
    'spec/mpa',
    {
      title: 'MPA: the Mabat Persistence Architecture',
      description: "Mabat's contract as numbered rules: what a view declares, how it loads, is overridden, served and written.",
      tableOfContents: { maxHeadingLevel: 3 },
    },
    spec,
  );
}

write(
  'design',
  {
    title: 'Design document',
    description: 'The goals of Mabat, the reasoning behind its architecture, and what each milestone built.',
  },
  rewriteLinks(withoutTitle(read('docs/design.md')), 'docs', 'design'),
);

write(
  'changelog',
  { title: 'Changelog', description: 'What each version of Mabat adds.' },
  rewriteLinks(withoutTitle(read('CHANGELOG.md')), '', 'changelog'),
);

// The reference pages, from the index of the specification
const index = JSON.parse(read('docs/mpa.json'));
const rules = (from, ids) => (ids.length ? ids.map((id) => ruleLink(from, id)).join(', ') : '—');
const cell = (text) => text.replace(/\|/g, '\\|');

write(
  'reference/capabilities',
  {
    title: 'Capabilities',
    description: 'Everything Mabat does, with the API that does it and the rules that define it.',
  },
  `Each capability lists the API that provides it and the [MPA](${relative('reference/capabilities', 'spec/mpa')}) rules that
define it. The same list is in [\`docs/mpa.json\`](${github}/docs/mpa.json) for tools.

| Capability | What it does | API | Rules |
| --- | --- | --- | --- |
${index.capabilities
  .map((c) => `| **${c.id}** | ${cell(c.summary)} | ${c.api.map((a) => `\`${cell(a)}\``).join('<br/>')} | ${rules('reference/capabilities', c.rules)} |`)
  .join('\n')}

## Not supported

${index.unsupported.map((r) => `- ${ruleLink('reference/capabilities', r.id)} ${r.text}`).join('\n')}
`,
);

write(
  'reference/attributes',
  {
    title: 'Attributes',
    description: 'Every #[view(...)] attribute of #[derive(View)], where it goes and what it means.',
  },
  `Every attribute \`#[derive(View)]\` accepts. A test fails when the derive accepts one that is not listed here.

| Attribute | On | Syntax | Meaning | Rules |
| --- | --- | --- | --- | --- |
${index.attributes
  .map((a) => `| \`${a.name}\` | ${a.on} | \`${cell(a.syntax)}\` | ${cell(a.summary)} | ${rules('reference/attributes', a.rules)} |`)
  .join('\n')}

## Functions

| API | What it does | Rules |
| --- | --- | --- |
${index.functions.map((f) => `| \`${cell(f.api)}\` | ${cell(f.summary)} | ${rules('reference/attributes', f.rules)} |`).join('\n')}
`,
);

write(
  'reference/errors',
  {
    title: 'Errors and diagnostics',
    description: 'Every variant of mabat::Error, and the diagnostic codes of the override checks.',
  },
  `Every failure is a \`mabat::Error\`, whose message names the view and the path. A test fails when the crate has a
variant that is not listed here.

| Error | Rules |
| --- | --- |
${index.errors.map((e) => `| \`${e.variant}\` | ${rules('reference/errors', e.rules)} |`).join('\n')}

## Diagnostics

The checks of a registry (\`check\`, \`build\`, \`reload\`) and \`mabat check\` report diagnostics, each with a severity,
the view, the query, the file and line, and notes.

| Code | Severity | Meaning |
| --- | --- | --- |
${index.diagnostics.map((d) => `| \`${d.code}\` | ${d.severity} | ${cell(d.summary)} |`).join('\n')}
`,
);

// The architecture document: a page for its cover and one for each of its pages, from the HTML that
// docs/architecture/overview/build.sh renders to a PDF, with its figures and the PDF beside them
const architecture = (() => {
  const dir = 'docs/architecture/overview';
  const html = read(`${dir}/mabat-architecture.html`);
  const inner = (source, pattern) => source.match(pattern)?.[1].trim() ?? '';
  const sections = html.split('<section class="page').slice(1);
  const slugOf = (title) =>
    title
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-|-$/g, '');

  // Rule citations become links to the specification, from a page at `from`
  const cite = (text, from) =>
    text.replace(/<span class="r">(MPA-[A-Z]+-\d+)<\/span>/g, (_, id) => {
      return `<a class="rule-ref" href="${relative(from, 'spec/mpa')}#${anchor(id)}">${id}</a>`;
    });
  // One HTML block per line, so Markdown leaves each alone
  const block = (text) => text.replace(/\s*\n\s*/g, ' ').trim();

  const outPublic = join(site, 'public', 'architecture');
  rmSync(outPublic, { recursive: true, force: true });
  mkdirSync(join(outPublic, 'diagrams'), { recursive: true });
  for (const file of readdirSync(join(repo, dir, 'diagrams'))) {
    copyFileSync(join(repo, dir, 'diagrams', file), join(outPublic, 'diagrams', file));
  }
  copyFileSync(join(repo, dir, 'mabat-architecture.pdf'), join(outPublic, 'mabat-architecture.pdf'));

  const pages = sections.slice(1).map((section, i) => {
    const title = inner(section, /<h1>([\s\S]*?)<\/h1>/);
    return { section, title, slug: `architecture/${String(i + 1).padStart(2, '0')}-${slugOf(title)}` };
  });

  // The cover
  const cover = sections[0];
  const from = 'architecture/index';
  const toc = [...cover.matchAll(/<div><b>(\d+) · ([\s\S]*?)<\/b> — ([\s\S]*?)<\/div>/g)].map(
    ([, n, title, text]) => `| ${n} | [${title}](${relative(from, pages[n - 1].slug)}) | ${block(text).replace(/\|/g, '\\|')} |`,
  );
  write(
    from,
    {
      title: 'Architecture',
      description: 'How Mabat loads, serves and saves typed aggregates, in ten figures and a table.',
      sidebar: { label: 'Overview', order: 0 },
      tableOfContents: false,
    },
    `<p class="arch-lead">${block(cite(inner(cover, /<p class="sub">([\s\S]*?)<\/p>/), from))}</p>

<div class="arch-download"><a href="mabat-architecture.pdf">Download the PDF</a> <span>A3 landscape, ${sections.length} pages, for print and for review</span></div>

<div class="arch-notice">${block(cite(inner(cover, /<div class="notice">([\s\S]*?)<\/div>/), from))}</div>

${[...inner(cover, /<div class="meta">([\s\S]*?)<\/div>/).matchAll(/<p>([\s\S]*?)<\/p>/g)]
  .map(([, p]) => `<p>${block(cite(p, from))}</p>`)
  .join('\n\n')}

## Contents

| | Page | What it shows |
| --- | --- | --- |
${toc.join('\n')}
`,
  );

  // The pages
  pages.forEach(({ section, title, slug }, i) => {
    const kicker = inner(section, /<span class="kicker">([\s\S]*?)<\/span>/);
    const dek = inner(section, /<p class="dek">([\s\S]*?)<\/p>/);
    const img = section.match(/<img src="([^"]+)" alt="([^"]*)">/);
    const caption = inner(section, /<figcaption>([\s\S]*?)<\/figcaption>/);
    const table = inner(section, /(<table class="m">[\s\S]*?<\/table>)/);
    const after = inner(section, /<div class="after">([\s\S]*?)<\/div>/);
    const paragraphs = (text) =>
      [...text.matchAll(/<p>([\s\S]*?)<\/p>/g)].map(([, p]) => `<p>${block(cite(p, slug))}</p>`).join('\n\n');
    const svg = img && `${relative(slug, 'architecture')}${img[1]}`;
    const body = [
      `<p class="arch-kicker">${block(kicker)}</p>`,
      `<p class="arch-lead">${block(cite(dek, slug))}</p>`,
      img &&
        `<figure class="arch-figure"><a href="${svg}" title="Open the figure on its own"><img src="${svg}" alt="${img[2]}" /></a></figure>`,
      table && `<div class="arch-table">${block(cite(table, slug))}</div>`,
      paragraphs(caption || after),
    ].filter(Boolean);
    write(
      slug,
      {
        title,
        description: block(dek.replace(/<[^>]+>/g, '')).replace(/&amp;/g, '&').replace(/&lt;/g, '<').replace(/&gt;/g, '>'),
        sidebar: { label: `${i + 1} · ${title}`, order: i + 1 },
        tableOfContents: false,
        pagefind: true,
      },
      body.join('\n\n'),
    );
  });
  return pages.length;
})();

console.log(
  `synced: spec (${index.rules.length} rules), design, changelog, reference, architecture (${architecture} pages)`,
);
