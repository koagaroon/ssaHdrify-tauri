import { useId, useMemo, useState } from "react";
import { useI18n } from "./i18n/useI18n";
import { licensePages, licenseSections } from "./lib/license-pages";

export default function LicenseText({
  text,
  components = false,
}: {
  text: string;
  components?: boolean;
}): React.ReactElement {
  const { t } = useI18n();
  const selectId = useId();
  const [sectionIndex, setSectionIndex] = useState(0);
  const [pageIndex, setPageIndex] = useState(0);
  const sections = useMemo(
    () => (components ? licenseSections(text) : [{ title: "", text }]),
    [components, text]
  );
  const section = sections[sectionIndex] ?? sections[0]!;
  const pages = useMemo(() => licensePages(section.text), [section.text]);
  const currentPage = Math.min(pageIndex, pages.length - 1);

  return (
    <div className="license-reader">
      {components && (
        <div className="license-component-picker">
          <label htmlFor={selectId}>{t("licenses_component")}</label>
          <select
            id={selectId}
            value={sectionIndex}
            onChange={(event) => {
              setSectionIndex(Number(event.target.value));
              setPageIndex(0);
            }}
          >
            {sections.map((item, index) => (
              <option key={index} value={index}>
                {item.title}
              </option>
            ))}
          </select>
        </div>
      )}
      {pages.length > 1 && (
        <div className="license-pagination">
          <button
            type="button"
            disabled={currentPage === 0}
            onClick={() => setPageIndex(currentPage - 1)}
          >
            {t("licenses_previous")}
          </button>
          <span role="status">{t("licenses_page", currentPage + 1, pages.length)}</span>
          <button
            type="button"
            disabled={currentPage === pages.length - 1}
            onClick={() => setPageIndex(currentPage + 1)}
          >
            {t("licenses_next")}
          </button>
        </div>
      )}
      <pre
        key={`${sectionIndex}:${currentPage}`}
        className="license-full-text"
        tabIndex={0}
        aria-label={components ? section.title : t("licenses_full_text")}
      >
        {pages[currentPage]}
      </pre>
    </div>
  );
}
