import { useState } from "react";
import HdrConvert from "./features/hdr-convert/HdrConvert";
import TimingShift from "./features/timing-shift/TimingShift";
import FontEmbed from "./features/font-embed/FontEmbed";

type Tab = "hdr" | "timing" | "fonts";

const TABS: { id: Tab; label: string; desc: string }[] = [
  { id: "hdr", label: "HDR Convert", desc: "SDR → HDR color space conversion" },
  { id: "timing", label: "Timing Shift", desc: "Batch subtitle timing adjustment" },
  { id: "fonts", label: "Font Embed", desc: "Subset & embed fonts into ASS" },
];

function App() {
  const [activeTab, setActiveTab] = useState<Tab>("hdr");

  return (
    <div className="flex flex-col h-screen bg-neutral-950 text-neutral-100">
      {/* ── Header ─────────────────────────────────── */}
      <header className="flex-none border-b border-neutral-800 bg-neutral-900/80 backdrop-blur-sm">
        <div className="px-5 pt-4 pb-0">
          <h1 className="text-lg font-semibold tracking-tight">
            SSA HDRify
          </h1>
          <nav className="flex gap-1 mt-3" role="tablist">
            {TABS.map((tab) => (
              <button
                key={tab.id}
                role="tab"
                aria-selected={activeTab === tab.id}
                onClick={() => setActiveTab(tab.id)}
                className={`
                  px-4 py-2 text-sm font-medium rounded-t-lg transition-colors
                  ${
                    activeTab === tab.id
                      ? "bg-neutral-950 text-white border-t border-x border-neutral-700"
                      : "text-neutral-400 hover:text-neutral-200 hover:bg-neutral-800/50"
                  }
                `}
              >
                {tab.label}
              </button>
            ))}
          </nav>
        </div>
      </header>

      {/* ── Tab content ────────────────────────────── */}
      <main className="flex-1 overflow-y-auto p-5">
        {activeTab === "hdr" && <HdrConvert />}
        {activeTab === "timing" && <TimingShift />}
        {activeTab === "fonts" && <FontEmbed />}
      </main>

      {/* ── Footer ─────────────────────────────────── */}
      <footer className="flex-none px-5 py-2 text-xs text-neutral-500 border-t border-neutral-800">
        SSA HDRify v0.1.0
      </footer>
    </div>
  );
}

export default App;
