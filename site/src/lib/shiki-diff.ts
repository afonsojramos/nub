import type { ShikiTransformer } from '@shikijs/types';
import type { Element, Text } from 'hast';

/* A shiki transformer that gives ```diff fences a GitHub-style diff look: the
   leading `+`/`-` marker is REMOVED from the rendered text (it only decides the
   line's tone), and the line is tagged so `global.css` can paint a subtle
   add/remove background. Without this, shiki's `diff` grammar leaves the literal
   `+`/`-` in the text and colors the whole line bright green/red — which reads as
   a marker column, not a diff highlight.

   SCOPE: language-gated to `diff` fences only; every other block is untouched.

   Per line, the first character decides: `+` → `data-diff="add"`, `-` →
   `data-diff="remove"`, anything else (context) is left alone. The marker (and
   the single space that conventionally follows it) is stripped from the hast so
   the body aligns with the context lines. Colors live in `global.css`. */

function lineText(node: Element): string {
  let out = '';
  for (const child of node.children) {
    if (child.type === 'text') out += child.value;
    else if (child.type === 'element') out += lineText(child);
  }
  return out;
}

// Drop the first `count` characters from the line's hast, walking token spans
// left-to-right (the marker can fall in the first text token of the first span).
function stripLeading(node: Element, count: number): void {
  let remaining = count;
  for (const child of node.children) {
    if (remaining === 0) break;
    if (child.type !== 'element') continue;
    for (const grandchild of child.children) {
      if (remaining === 0) break;
      if (grandchild.type !== 'text') continue;
      const t = grandchild as Text;
      const take = Math.min(remaining, t.value.length);
      t.value = t.value.slice(take);
      remaining -= take;
    }
  }
}

export function transformerDiff(): ShikiTransformer {
  return {
    name: 'nub:diff',
    line(node) {
      if (this.options.lang !== 'diff') return;
      const text = lineText(node);
      const marker = text[0];
      if (marker !== '+' && marker !== '-') return;
      node.properties['data-diff'] = marker === '+' ? 'add' : 'remove';
      // Strip the marker plus the single conventional space after it, so the
      // line body lines up with the unmarked context lines.
      stripLeading(node, text[1] === ' ' ? 2 : 1);
    },
  };
}
