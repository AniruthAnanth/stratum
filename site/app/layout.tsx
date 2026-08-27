import type { Metadata, Viewport } from "next";
import type { ReactNode } from "react";
import { Bricolage_Grotesque } from "next/font/google";
import "./globals.css";

// next/font self-hosts the woff2 at build time: zero external font requests
// on the live page. Exposed as --font-grotesk for globals.css.
const bricolage = Bricolage_Grotesque({
  subsets: ["latin"],
  display: "swap",
  variable: "--font-grotesk",
});

const SITE_URL = "https://aniruthananth.github.io/stratum/";
const DESCRIPTION =
  "An open-source, Stata-compatible statistical IDE. Your do-files, a new engine, zero dollars.";

export const metadata: Metadata = {
  title: "Stratum",
  description: DESCRIPTION,
  openGraph: {
    title: "Stratum",
    description: DESCRIPTION,
    url: SITE_URL,
    siteName: "Stratum",
    images: [
      {
        url: `${SITE_URL}og.png`,
        width: 1200,
        height: 630,
        alt: "Stratum — Free. Stata charges $3,000 a seat.",
      },
    ],
    type: "website",
  },
  twitter: {
    card: "summary_large_image",
    title: "Stratum",
    description: DESCRIPTION,
    images: [`${SITE_URL}og.png`],
  },
};

export const viewport: Viewport = {
  themeColor: "#fbfbf9",
};

// Runs before first paint. `html.js` is the hook for the pre-paint dim state
// in globals.css; it is set only when JS is on AND motion is not reduced, so
// reduced-motion users are never stuck with grey words or width-0 chips.
const PRE_PAINT =
  "try{if(!matchMedia('(prefers-reduced-motion: reduce)').matches)document.documentElement.classList.add('js')}catch(e){}";

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en" className={bricolage.variable} suppressHydrationWarning>
      <body>
        <script dangerouslySetInnerHTML={{ __html: PRE_PAINT }} />
        {/* The CTA is docked 46px off-screen in CSS (see .cta-slide); no JS means nothing would slide it in. */}
        <noscript>
          <style>{".cta-slide{transform:none!important}"}</style>
        </noscript>
        {children}
      </body>
    </html>
  );
}
