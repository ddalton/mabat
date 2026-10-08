// Writes the pages of the site that come from the repository: the MPA specification, the design
// document and the changelog from their Markdown, and the reference pages from docs/mpa.json.
// The repository files stay the source of truth; the written pages are not committed.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
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
  const depth = from === 'index' ? 0 : from.split('/').length;
  return `${'../'.repeat(depth)}${to === 'index' ? '' : `${to}/`}`;
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

console.log(`synced: spec (${index.rules.length} rules), design, changelog, reference`);
