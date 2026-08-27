"use client";

/**
 * The statement: 37 words and four chips, inked in by scroll.
 *
 * Every unit (word, chip, the "2005." pop) owns a window of one scroll clock —
 * `useScroll` over the paragraph, offset ["start 0.55", "end 0.62"] — so the
 * reveal completes at ~90% of max scroll and chips never animate amid grey
 * neighbours. Words write color only (MotionValue style, no re-render, no
 * layout). Chips latch once when the sweep reaches their window:
 *
 *   - pop chips (mark, do-file) scale 0.5 -> 1 with the single overshoot curve;
 *   - unfurl chips (terminal, regression card) animate width 0 -> auto on the
 *     expo-out curve, and only when that completes do their contents play;
 *   - "2005." gets the biggest pop (scale 0.4, rotate -6deg, 0.7s) and inks
 *     from the same progress value, so scale and ink land together.
 *
 * Without JS, or with reduced motion, `enhanced` stays false: plain spans,
 * full ink, chips printed whole. app/globals.css carries the matching
 * pre-paint (`html.js`) and reduced-motion rules.
 */

import {
  Fragment,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  motion,
  useScroll,
  useTransform,
  useMotionValueEvent,
  useReducedMotion,
  type MotionValue,
} from "motion/react";

const INK = "rgba(35, 35, 35, 1)";
const INK_40 = "rgba(35, 35, 35, 0.4)";

/** The page's one overshoot. Canonical source — there is no CSS twin. */
const EASE_POP = [0.68, -0.55, 0.27, 1.55] as const;
/** Expo-out: slides, unfurls, fades. */
const EASE_EXPO = [0.19, 1, 0.22, 1] as const;

type ChipKind = "mark" | "dofile" | "term" | "reg";

type Unit =
  | { kind: "word"; text: string }
  | { kind: "pop"; text: string }
  | { kind: "chip"; chip: ChipKind };

const word = (text: string): Unit => ({ kind: "word", text });

const UNITS: Unit[] = [
  { kind: "chip", chip: "mark" },
  word("Stratum"),
  word("is"),
  word("an"),
  word("open-source"),
  word("statistical"),
  word("IDE."),
  word("It"),
  word("runs"),
  word("your"),
  word("Stata"),
  word("do-files"),
  { kind: "chip", chip: "dofile" },
  word("on"),
  word("an"),
  word("engine"),
  word("written"),
  word("in"),
  word("Rust."),
  { kind: "chip", chip: "term" },
  word("Same"),
  word("commands."),
  word("Same"),
  word("results."),
  { kind: "chip", chip: "reg" },
  word("Free."),
  word("Stata"),
  word("charges"),
  word("$3,000"),
  word("a"),
  word("seat."),
  word("AI-native,"),
  word("and"),
  word("it"),
  word("doesn't"),
  word("look"),
  word("like"),
  word("it"),
  word("shipped"),
  word("in"),
  { kind: "pop", text: "2005." },
];

type Range = [number, number];

interface UnitProps {
  progress: MotionValue<number>;
  range: Range;
  enhanced: boolean;
}

function WordUnit({
  text,
  progress,
  range,
  enhanced,
}: UnitProps & { text: string }) {
  const color = useTransform(progress, range, [INK_40, INK]);
  return (
    <motion.span className="word" style={enhanced ? { color } : undefined}>
      {text}
    </motion.span>
  );
}

function PopUnit({
  text,
  progress,
  range,
  enhanced,
}: UnitProps & { text: string }) {
  const color = useTransform(progress, range, [INK_40, INK]);
  const [popped, setPopped] = useState(false);
  useMotionValueEvent(progress, "change", (v) => {
    if (v >= range[0]) setPopped(true);
  });
  if (!enhanced) return <span className="word pop">{text}</span>;
  return (
    <motion.span
      className="word pop"
      style={{ color }}
      initial={{ scale: 0.4, rotate: -6 }}
      animate={popped ? { scale: 1, rotate: 0 } : { scale: 0.4, rotate: -6 }}
      transition={{ duration: 0.7, ease: EASE_POP }}
    >
      {text}
    </motion.span>
  );
}

interface ChipState {
  /** The wrapper has started its entrance. */
  shown: boolean;
  /** The frame is fully open; contents may play (unfurl) — equals `shown` for pops. */
  open: boolean;
}

function ChipUnit({
  mode,
  progress,
  range,
  enhanced,
  children,
}: UnitProps & {
  mode: "pop" | "unfurl";
  children: (state: ChipState) => ReactNode;
}) {
  const [shown, setShown] = useState(false);
  const [open, setOpen] = useState(false);
  useMotionValueEvent(progress, "change", (v) => {
    if (v >= range[0]) setShown(true);
  });
  // A load that lands mid-page never sees a "change" past the threshold.
  useEffect(() => {
    if (enhanced && progress.get() >= range[0]) setShown(true);
  }, [enhanced, progress, range]);

  if (!enhanced) {
    return (
      <span className={`chipwrap chip-${mode}`}>
        {children({ shown: true, open: true })}
      </span>
    );
  }
  if (mode === "pop") {
    return (
      <motion.span
        className="chipwrap chip-pop"
        initial={{ scale: 0.5, y: 6, opacity: 0 }}
        animate={
          shown ? { scale: 1, y: 0, opacity: 1 } : { scale: 0.5, y: 6, opacity: 0 }
        }
        transition={{
          duration: 0.5,
          ease: EASE_POP,
          opacity: { duration: 0.25, ease: "easeOut" },
        }}
      >
        {children({ shown, open: shown })}
      </motion.span>
    );
  }
  return (
    <motion.span
      className={`chipwrap chip-unfurl${open ? " settled" : ""}`}
      initial={{ width: 0 }}
      animate={shown ? { width: "auto" } : { width: 0 }}
      transition={{ duration: 0.55, ease: EASE_EXPO }}
      onAnimationComplete={(definition) => {
        if (
          typeof definition === "object" &&
          definition !== null &&
          (definition as { width?: unknown }).width === "auto"
        ) {
          setOpen(true);
        }
      }}
    >
      {children({ shown, open })}
    </motion.span>
  );
}

function Pill({ text }: { text: string }) {
  return (
    <span className="pill" aria-hidden="true">
      {text}
    </span>
  );
}

const MARK_BARS = [
  { y: 4, width: 28 },
  { y: 13.5, width: 20 },
  { y: 23, width: 12 },
];

function Mark({ enhanced, shown }: { enhanced: boolean; shown: boolean }) {
  return (
    <svg className="mark" viewBox="0 0 32 32" aria-hidden="true" focusable="false">
      {MARK_BARS.map((bar, i) =>
        enhanced ? (
          <motion.rect
            key={i}
            x="2"
            y={bar.y}
            width={bar.width}
            height="7"
            rx="3.5"
            fill="#116a6a"
            initial={{ opacity: 0 }}
            animate={shown ? { opacity: 1 } : { opacity: 0 }}
            transition={{ duration: 0.3, ease: "easeOut", delay: 0.06 + 0.04 * i }}
          />
        ) : (
          <rect
            key={i}
            x="2"
            y={bar.y}
            width={bar.width}
            height="7"
            rx="3.5"
            fill="#116a6a"
          />
        ),
      )}
    </svg>
  );
}

function DofileChip({ enhanced, shown }: { enhanced: boolean; shown: boolean }) {
  return (
    <span
      className="chip dofile"
      role="img"
      tabIndex={0}
      aria-label="A plain do-file, cursor blinking"
    >
      {enhanced ? (
        <motion.span
          aria-hidden="true"
          initial={{ opacity: 0 }}
          animate={shown ? { opacity: 1 } : { opacity: 0 }}
          transition={{ duration: 0.25, ease: "easeOut", delay: 0.08 }}
        >
          auto.do
        </motion.span>
      ) : (
        <span aria-hidden="true">auto.do</span>
      )}
      <span className="d-caret" aria-hidden="true" />
      <Pill text="a plain .do file" />
    </span>
  );
}

/** `static`: no JS/reduced motion, printed whole. `armed`: frame open, caret
 *  on, nothing typed. `play`: CSS typing + output (see globals.css). */
function TermChip({ enhanced, play }: { enhanced: boolean; play: boolean }) {
  return (
    <span
      className={`chip term ${enhanced ? (play ? "play" : "armed") : "static"}`}
      role="img"
      tabIndex={0}
      aria-label="Stratum terminal running summarize price: 74 observations, mean 6165.257"
    >
      <span className="t-line t-cmd" aria-hidden="true">
        <span className="t-prompt">{". "}</span>
        <span className="t-typed">summarize price</span>
        <span className="t-caret" />
      </span>
      <span className="t-line t-out" aria-hidden="true">
        {"Variable     Obs      Mean"}
      </span>
      <span className="t-line t-out t-out2" aria-hidden="true">
        {"   price      74  6165.257"}
      </span>
      <Pill text="runs on the real engine" />
    </span>
  );
}

/** Real auto.dta numbers: regress price mpg weight. Keep them real forever. */
function RegChip({ enhanced, open }: { enhanced: boolean; open: boolean }) {
  const rows: { cls: string; node: ReactNode }[] = [
    { cls: "r-head", node: ". regress price mpg weight" },
    {
      cls: "r-row",
      node: (
        <>
          <span>mpg</span>
          <span className="r-num">−49.51</span>
        </>
      ),
    },
    {
      cls: "r-row",
      node: (
        <>
          <span>weight</span>
          <span className="r-num">1.75</span>
        </>
      ),
    },
    // En spaces (U+2002) around the middot: wider than a word space in mono.
    { cls: "r-foot", node: <>{"N = 74\u2002·\u2002R² = 0.29"}</> },
  ];
  return (
    <span
      className="chip reg"
      role="img"
      tabIndex={0}
      aria-label="Regression of price on mpg and weight: mpg −49.51, weight 1.75, 74 observations, R-squared 0.29"
    >
      {rows.map((row, i) =>
        enhanced ? (
          <motion.span
            key={i}
            className={row.cls}
            aria-hidden="true"
            initial={{ opacity: 0, y: 6 }}
            animate={open ? { opacity: 1, y: 0 } : { opacity: 0, y: 6 }}
            transition={{ duration: 0.35, ease: EASE_EXPO, delay: 0.04 * i }}
          >
            {row.node}
          </motion.span>
        ) : (
          <span key={i} className={row.cls} aria-hidden="true">
            {row.node}
          </span>
        ),
      )}
      <Pill text="auto.dta · N = 74" />
    </span>
  );
}

export default function Statement() {
  const ref = useRef<HTMLParagraphElement>(null);
  const reduced = useReducedMotion();
  const [enhanced, setEnhanced] = useState(false);
  useEffect(() => {
    if (!reduced) setEnhanced(true);
  }, [reduced]);

  const { scrollYProgress } = useScroll({
    target: ref,
    offset: ["start 0.55", "end 0.62"],
  });
  const count = UNITS.length;

  return (
    <div className="flow">
      <p className="statement" ref={ref}>
        {UNITS.map((unit, i) => {
          const start = i / count;
          const range: Range = [start, Math.min(start + 2.4 / count, 1)];
          return (
            <Fragment key={i}>
              {unit.kind === "word" && (
                <WordUnit
                  text={unit.text}
                  progress={scrollYProgress}
                  range={range}
                  enhanced={enhanced}
                />
              )}
              {unit.kind === "pop" && (
                <PopUnit
                  text={unit.text}
                  progress={scrollYProgress}
                  range={range}
                  enhanced={enhanced}
                />
              )}
              {unit.kind === "chip" && (
                <ChipUnit
                  mode={unit.chip === "term" || unit.chip === "reg" ? "unfurl" : "pop"}
                  progress={scrollYProgress}
                  range={range}
                  enhanced={enhanced}
                >
                  {({ shown, open }) => (
                    <>
                      {unit.chip === "mark" && <Mark enhanced={enhanced} shown={shown} />}
                      {unit.chip === "dofile" && (
                        <DofileChip enhanced={enhanced} shown={shown} />
                      )}
                      {unit.chip === "term" && <TermChip enhanced={enhanced} play={open} />}
                      {unit.chip === "reg" && <RegChip enhanced={enhanced} open={open} />}
                    </>
                  )}
                </ChipUnit>
              )}{" "}
            </Fragment>
          );
        })}
      </p>
      {/* En spaces (U+2002) around the middots, letter-spaced mono. */}
      <p className="platforms">{"macOS\u2002·\u2002Windows\u2002·\u2002Linux"}</p>
    </div>
  );
}
