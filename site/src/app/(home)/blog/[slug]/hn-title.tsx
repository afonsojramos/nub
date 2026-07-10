'use client';

import { useEffect, useState } from 'react';

/**
 * Headline that swaps to an alternate title when the page is visited with
 * `?hn` (Hacker News submissions). Reads window.location in an effect rather
 * than useSearchParams so the statically prerendered route needs no Suspense
 * boundary; the swap happens post-hydration, which is fine for this use.
 */
export function HnTitle({ title, hnTitle }: { title: string; hnTitle: string }) {
  const [text, setText] = useState(title);

  useEffect(() => {
    if (!new URLSearchParams(window.location.search).has('hn')) return;
    setText(hnTitle);
    const full = `${hnTitle} — Nub`;
    document.title = full;
    // Next re-applies the metadata <title> once shortly after hydration,
    // clobbering the line above — watch and re-assert for as long as the
    // post is mounted.
    const observer = new MutationObserver(() => {
      if (document.title !== full) document.title = full;
    });
    observer.observe(document.head, {
      subtree: true,
      childList: true,
      characterData: true,
    });
    return () => observer.disconnect();
  }, [hnTitle]);

  return <>{text}</>;
}
