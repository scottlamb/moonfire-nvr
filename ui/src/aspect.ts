// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

/**
 * Sets CSS properties on <tt>innerRef</tt> to fill as much of <tt>rect</tt>
 * as possible while maintaining aspect ratio.
 *
 * Uses imperative sizing rather than the CSS <tt>aspect-ratio</tt> property
 * because the video's display aspect ratio comes from the server (based on the
 * init segment), not from the element's natural dimensions. The caller should
 * use a ResizeObserver to keep the sizing up to date.
 */
export function fillAspect(
  rect: DOMRectReadOnly,
  innerRef: React.RefObject<HTMLElement | null>,
  aspect: [number, number],
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
