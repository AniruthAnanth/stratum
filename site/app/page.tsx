import Statement from "@/components/Statement";
import CtaDock from "@/components/CtaDock";

export default function Page() {
  return (
    <main>
      <h1 className="sr-only">Stratum — an open-source Stata alternative</h1>
      <Statement />
      <CtaDock />
    </main>
  );
}
