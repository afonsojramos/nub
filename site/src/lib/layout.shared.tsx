import type {
  BaseLayoutProps,
  LinkItemType,
} from 'fumadocs-ui/layouts/shared';
import { GitHubStarPill } from '@/components/github-star-pill';

/* The wordmark — stylized with a trailing period as a logo. */
export function Wordmark() {
  return (
    <span className="font-display text-lg font-medium tracking-tight text-fd-foreground">
      nub<span className="text-ember">.</span>
    </span>
  );
}

/* The GitHub entry, rendered as a star-button pill pinned to the nav's secondary
   (top-right) slot. Replaces fumadocs' default `githubUrl` icon so there's no
   duplicate GitHub control. Exported so the docs/guides layouts — which drop the
   primary nav links — can still surface it on their own headers.

   The top bar stays CLEAN: the optional "Leave a star" nudge does NOT live here.
   It is an absolutely-positioned annotation overlaid on the HOME hero (see
   `StarNudge`, mounted in the hero), so the bar is never widened by it. */
export function githubPillLink(): LinkItemType {
  return {
    type: 'custom',
    secondary: true,
    children: <GitHubStarPill repo="nubjs/nub" />,
  };
}

export function baseOptions(): BaseLayoutProps {
  return {
    nav: {
      title: <Wordmark />,
    },
    links: [
      { text: 'Docs', url: '/docs', active: 'nested-url' },
      { text: 'Blog', url: '/blog', active: 'nested-url' },
      githubPillLink(),
    ],
    themeSwitch: { enabled: true },
  };
}
