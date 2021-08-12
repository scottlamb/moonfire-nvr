// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

/**
 * Sets CSS properties on <tt>innerRef</tt> to fill as much of <tt>rect</tt>
 * as possible while maintaining aspect ratio.
 *
 * While Chrome 89 supports the "aspect-ratio" CSS property and behaves in a
 * predictable way, Firefox 87 doesn't. Emulating it with an <img> child
 * doesn't work well either for using a (flex item) ancestor's (calculated)
 * height to compute the <img>'s width and then the parent's width. There are
 * open bugs that look related, eg:
 * https://bugzilla.mozilla.org/show_bug.cgi?id=1349738
 * https://bugzilla.mozilla.org/show_bug.cgi?id=1690423
 * so just do it all by hand. The caller should use a ResizeObserver.
 */
export function fillAspect(
  rect: DOMRectReadOnly,
  innerRef: React.RefObject<HTMLElement>,
  aspect: [number, number]
) {
  const w = rect.width;
  const h = rect.height;
  const hFromW = (w * aspect[1]) / aspect[0];
  const inner = innerRef.current;
  if (inner === null) {
    return;
  }
  if (hFromW > h) {
    inner.style.width = `${(h * aspect[0]) / aspect[1]}px`;
    inner.style.height = `${h}px`;
  } else {
    inner.style.width = `${w}px`;
    inner.style.height = `${hFromW}px`;
  }
}
