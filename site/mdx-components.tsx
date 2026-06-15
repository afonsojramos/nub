import defaultMdxComponents from 'fumadocs-ui/mdx';
import type { MDXComponents } from 'mdx/types';
import { Bench } from '@/components/code';
import { InstallTabs } from '@/components/install-tabs';

export function getMDXComponents(components?: MDXComponents): MDXComponents {
  return {
    ...defaultMdxComponents,
    Bench,
    InstallTabs,
    ...components,
  };
}
