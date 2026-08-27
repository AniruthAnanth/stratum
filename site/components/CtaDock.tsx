"use client";

/**
 * The docked "Star on GitHub" pill.
 *
 * Starts 46px half-off the bottom (CSS, so the first JS frame and framer agree
 * and there is no jump) and slides in over page progress [0.7, 0.95] on the
 * expo-out curve — a slide, so no overshoot; "2005." owns the only one.
 * Without JS the <noscript> style in app/layout.tsx docks it fully; with
 * reduced motion `enhanced` stays false and the CSS transform is not applied.
 */

import { useEffect, useState } from "react";
import {
  cubicBezier,
  motion,
  useScroll,
  useTransform,
  useReducedMotion,
} from "motion/react";

const easeExpo = cubicBezier(0.19, 1, 0.22, 1);

export default function CtaDock() {
  const reduced = useReducedMotion();
  const [enhanced, setEnhanced] = useState(false);
  useEffect(() => {
    if (!reduced) setEnhanced(true);
  }, [reduced]);

  const { scrollYProgress } = useScroll();
  const y = useTransform(scrollYProgress, [0.7, 0.95], [46, 0], { ease: easeExpo });

  return (
    <div className="cta-dock">
      <motion.span className="cta-slide" style={enhanced ? { y } : undefined}>
        <a className="cta" href="https://github.com/AniruthAnanth/stratum">
          <svg
            width="18"
            height="18"
            viewBox="0 0 16 16"
            fill="currentColor"
            aria-hidden="true"
          >
            <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27s1.36.09 2 .27c1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8Z" />
          </svg>
          Star on GitHub
        </a>
      </motion.span>
    </div>
  );
}
