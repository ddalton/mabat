// Turn ```mermaid code blocks into <pre class="mermaid"> elements, which src/components/Head.astro
// renders as diagrams in the browser.
function escape(text) {
  return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

function visit(node, parent) {
  if (node.type === 'code' && node.lang === 'mermaid' && parent) {
    const index = parent.children.indexOf(node);
    parent.children[index] = { type: 'html', value: `<pre class="mermaid">${escape(node.value)}</pre>` };
    return;
  }
  for (const child of node.children ?? []) visit(child, node);
}

export function mermaidBlocks() {
  return (tree) => visit(tree, null);
}
