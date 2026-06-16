import type { Metadata } from 'next';
import { source } from '@/lib/source';
import { clampSubtitle } from '@/lib/og';
import {
  DocsPage,
  DocsBody,
  DocsDescription,
  DocsTitle,
} from 'fumadocs-ui/page';
import { notFound } from 'next/navigation';
import { AIActions } from '@/components/ai-actions';
import { getMDXComponents } from '../../../../mdx-components';

export default async function Page(props: {
  params: Promise<{ slug?: string[] }>;
}) {
  const params = await props.params;
  const page = source.getPage(params.slug);
  if (!page) notFound();

  const MDXContent = page.data.body;

  return (
    <DocsPage toc={page.data.toc} full={page.data.full}>
      <DocsTitle>{page.data.title}</DocsTitle>
      <DocsDescription>{page.data.description}</DocsDescription>
      <AIActions markdownUrl={`/llms${page.url}.mdx`} pageUrl={page.url} />
      {/* fumadocs' DocsPage emits no <main>/landmark; mark the prose body as the
          page's main landmark so the doc has exactly one (WCAG / Lighthouse). */}
      <DocsBody role="main">
        <MDXContent components={getMDXComponents()} />
      </DocsBody>
    </DocsPage>
  );
}

export function generateStaticParams() {
  return source.generateParams();
}

/* Pages mapping to a concrete command surface get the command spelling as the
   social-card eyebrow (matching the sidebar chips in docs/layout.tsx); others
   fall back to a plain "Documentation" label. */
const EYEBROW_BY_URL: Record<string, string> = {
  '/docs/runtime': 'nub <file>',
  '/docs/run': 'nub run',
  '/docs/nubx': 'nubx',
  '/docs/install': 'nub install',
  '/docs/node': 'nub node',
  '/docs/pm': 'nub pm',
  '/docs/watch': 'nub watch',
};

/* Build the per-page social-card URL handled by `app/og/route.tsx`. */
function ogImageUrl({
  url,
  title,
  description,
}: {
  url: string;
  title: string;
  description?: string;
}): string {
  const params = new URLSearchParams({
    title,
    eyebrow: EYEBROW_BY_URL[url] ?? 'Documentation',
  });
  const subtitle = clampSubtitle(description);
  if (subtitle) params.set('subtitle', subtitle);
  return `/og?${params.toString()}`;
}

export async function generateMetadata(props: {
  params: Promise<{ slug?: string[] }>;
}): Promise<Metadata> {
  const params = await props.params;
  const page = source.getPage(params.slug);
  if (!page) notFound();

  const { title, description } = page.data;
  const ogImage = ogImageUrl({ url: page.url, title, description });

  return {
    title,
    description,
    // Self-canonical: each docs page points at its own URL rather than
    // inheriting the root layout's `/` canonical.
    alternates: { canonical: page.url },
    openGraph: {
      type: 'article',
      url: page.url,
      title,
      description,
      images: [{ url: ogImage, width: 1200, height: 630, alt: title }],
    },
    twitter: {
      card: 'summary_large_image',
      title,
      description,
      images: [ogImage],
    },
  };
}
