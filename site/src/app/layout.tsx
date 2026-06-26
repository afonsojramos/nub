import './global.css';
import { RootProvider } from 'fumadocs-ui/provider/next';
import { Fraunces, Newsreader, IBM_Plex_Mono, Caveat } from 'next/font/google';
import type { ReactNode } from 'react';
import type { Metadata, Viewport } from 'next';

const fraunces = Fraunces({
  subsets: ['latin'],
  variable: '--font-fraunces',
  display: 'swap',
  // `opsz` drives the optical-size response used by the display headings; the
  // `SOFT`/`WONK` axes were configured but never read by any class or
  // font-variation-settings rule, so they only inflated the variable-font
  // download (render-blocking + LCP). Dropping them is a no-visual-change win.
  axes: ['opsz'],
  preload: true,
});

const newsreader = Newsreader({
  subsets: ['latin'],
  variable: '--font-newsreader',
  display: 'swap',
  style: ['normal', 'italic'],
});

const plexMono = IBM_Plex_Mono({
  subsets: ['latin'],
  variable: '--font-plex-mono',
  display: 'swap',
  weight: ['400', '500', '600'],
});

// Handwriting face — used ONLY by the optional "Leave a Star" nudge near the
// header pill. Gated behind that component's SHOW_STAR_NUDGE flag, so it costs
// nothing visually when the nudge is off (the font still preloads; if the nudge
// is permanently removed, drop this too).
const caveat = Caveat({
  subsets: ['latin'],
  variable: '--font-caveat',
  display: 'swap',
  weight: ['600'],
});

const TITLE = 'Nub — an all-in-one toolkit for Node.js';
const DESCRIPTION =
  'Nub is a TypeScript-first toolkit for Node.js: run TypeScript files on stock Node, a faster npm run, a pnpm-compatible package manager, and a built-in Node version manager. No lock-in.';
const SITE_URL = 'https://nubjs.com';

// Structured data: a SoftwareApplication (the CLI) plus the publishing
// Organization and the WebSite, so search engines can render a rich result and
// associate the docs/blog with the project. Emitted once, in the root layout.
const JSON_LD = {
  '@context': 'https://schema.org',
  '@graph': [
    {
      '@type': 'SoftwareApplication',
      '@id': `${SITE_URL}/#software`,
      name: 'Nub',
      description: DESCRIPTION,
      url: SITE_URL,
      applicationCategory: 'DeveloperApplication',
      operatingSystem: 'macOS, Linux, Windows',
      offers: { '@type': 'Offer', price: '0', priceCurrency: 'USD' },
      softwareRequirements: 'Node.js',
      author: { '@id': `${SITE_URL}/#org` },
    },
    {
      '@type': 'Organization',
      '@id': `${SITE_URL}/#org`,
      name: 'Nub',
      url: SITE_URL,
      sameAs: ['https://github.com/nubjs/nub'],
    },
    {
      '@type': 'WebSite',
      '@id': `${SITE_URL}/#website`,
      name: 'Nub',
      url: SITE_URL,
      publisher: { '@id': `${SITE_URL}/#org` },
    },
  ],
};

export const metadata: Metadata = {
  title: {
    default: TITLE,
    template: '%s — Nub',
  },
  description: DESCRIPTION,
  metadataBase: new URL(SITE_URL),
  applicationName: 'Nub',
  alternates: {
    canonical: '/',
  },
  icons: {
    icon: [
      { url: '/icon.svg', type: 'image/svg+xml' },
      { url: '/favicon.ico', sizes: '32x32' },
    ],
    apple: [{ url: '/apple-touch-icon.png', sizes: '180x180' }],
  },
  openGraph: {
    type: 'website',
    siteName: 'Nub',
    url: SITE_URL,
    title: TITLE,
    description: DESCRIPTION,
  },
  twitter: {
    card: 'summary_large_image',
    title: TITLE,
    description: DESCRIPTION,
  },
  robots: {
    index: true,
    follow: true,
  },
};

export const viewport: Viewport = {
  width: 'device-width',
  initialScale: 1,
  themeColor: [
    { media: '(prefers-color-scheme: light)', color: '#faf7f0' },
    { media: '(prefers-color-scheme: dark)', color: '#100f0d' },
  ],
  colorScheme: 'dark light',
};

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html
      lang="en"
      className={`${fraunces.variable} ${newsreader.variable} ${plexMono.variable} ${caveat.variable}`}
      suppressHydrationWarning
    >
      <body className="flex min-h-screen flex-col antialiased">
        <script
          type="application/ld+json"
          // Static, build-time JSON from a trusted local constant — safe to inline.
          dangerouslySetInnerHTML={{ __html: JSON.stringify(JSON_LD) }}
        />
        <RootProvider theme={{ defaultTheme: 'system', enableSystem: true }}>
          {children}
        </RootProvider>
      </body>
    </html>
  );
}
