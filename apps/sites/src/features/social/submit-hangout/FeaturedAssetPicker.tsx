import { useEventsCopy } from "@dcl/one-ui/apps/sites/events/locale";
import { Notice } from "@dcl/one-ui/apps/sites/events/Notice";
import { Button } from "@dcl/one-ui/apps/sites/shared/Button/Button";
import { GeneralSearch } from "@dcl/one-ui/foundations/icons";
import { useEffect, useState } from "react";
import { searchFeaturedAssets, type FeaturedAsset } from "@/data/lib/catalyst/landings/hangout-options";

import styles from "./FeaturedAssetPicker.module.css";
import form from "./SubmitHangout.module.css";

export default function FeaturedAssetPicker({ value, onChange }: { value: string; onChange: (urn: string) => void }) {
  const { text, errorText } = useEventsCopy();
  const [query, setQuery] = useState(value);
  const [label, setLabel] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [read, setRead] = useState<{ query: string; assets?: FeaturedAsset[]; error?: string } | null>(null);
  const search = query.trim();
  useEffect(() => {
    if (search.length < 3) return;
    const controller = new AbortController();
    const timer = setTimeout(() => {
      searchFeaturedAssets(search, controller.signal).then(
        (assets) => { if (!controller.signal.aborted) setRead({ query: search, assets }); },
        (error: unknown) => { if (!controller.signal.aborted) setRead({ query: search, error: error instanceof Error ? error.message : "Could not search the catalogue." }); },
      );
    }, 300);
    return () => { clearTimeout(timer); controller.abort(); };
  }, [search, attempt]);
  const current = read?.query === search ? read : null;
  const selected = current?.assets?.find((asset) => asset.urn === value);
  return <fieldset className={`${styles.root} ${form.picker}`}>
    <legend className={form.groupTitle}>{text("Featured item")}</legend>
    {value && <p className={`submit-hangout__hint ${styles.selected}`}>{text("Selected: {asset}", { asset: label ?? selected?.name ?? value })} <Button variant="tertiary" size="sm" onClick={() => { onChange(""); setQuery(""); setLabel(null); }}>{text("Remove featured asset")}</Button></p>}
    <div className={styles.search}>
      <label className={`submit-hangout__fieldlabel ${form.fieldLabel}`}><span className={form.fieldTitle}>{text("Item")}</span>
        <span className={styles.inputFrame}>
          <input className={`submit-hangout__field ${form.field} ${styles.input}`} value={query} onChange={(event) => setQuery(event.target.value)} placeholder={text("Search items or paste URN")} />
          <GeneralSearch className={styles.searchIcon} size={20} aria-hidden="true" />
        </span>
      </label>
      {current?.assets && current.assets.length > 0 && <ul className={styles.results} aria-label={text("Featured asset results")}>{current.assets.map((asset) => <li key={asset.urn}><button type="button" className={styles.asset} onClick={() => { onChange(asset.urn); setLabel(asset.name); setQuery(""); }}>
        {asset.thumbnail ? <img src={asset.thumbnail} className={styles.thumbnail} alt="" width={56} height={56} /> : <span className={styles.thumbnail} />}<span className={styles.label}><span className={styles.name}>{asset.name}</span><small className={styles.kind}>{text(asset.kind === "collection" ? "Collection" : "Item")}</small></span>
      </button></li>)}</ul>}
    </div>
    {search.length > 0 && search.length < 3 && <p className={`submit-hangout__hint ${form.fieldHint}`}>{text("Enter at least three characters.")}</p>}
    {search.length >= 3 && !current && <p role="status" className={form.fieldHint}>{text("Searching the catalogue…")}</p>}
    {current?.error && <Notice tone="error">{errorText(current.error)} <Button variant="tertiary" size="sm" onClick={() => setAttempt((previous) => previous + 1)}>{text("Retry asset search")}</Button></Notice>}
    {current?.assets?.length === 0 && <p className={`submit-hangout__hint ${form.fieldHint}`}>{text("No matching assets.")}</p>}
  </fieldset>;
}
