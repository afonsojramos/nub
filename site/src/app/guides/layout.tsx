import { DocsLayout } from 'fumadocs-ui/layouts/docs';
import type { ReactNode } from 'react';
import { baseOptions } from '@/lib/layout.shared';
import { guidesSource } from '@/lib/source';

export default function Layout({ children }: { children: ReactNode }) {
  // Drop the "Docs"/"Blog" nav links AND the GitHub pill from the guides sidebar
  // — the pill lives in the home nav only. Guides are a separate top-level
  // section, distinct from the docs sidebar.
  const { links, ...base } = baseOptions();

  return (
    <DocsLayout tree={guidesSource.pageTree} {...base} links={[]}>
      {children}
    </DocsLayout>
  );
}
