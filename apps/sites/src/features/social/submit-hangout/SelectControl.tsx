import { GeneralDownArrow } from "@dcl/one-ui/foundations/icons";
import type { ComponentProps } from "react";

import styles from "./SelectControl.module.css";

/** A native select with the editor's chevron; the caller's label names it. */
export default function SelectControl({ className, ...props }: ComponentProps<"select">) {
  return (
    <span className={styles.frame}>
      <select {...props} className={[styles.select, className].filter(Boolean).join(" ")} />
      <GeneralDownArrow className={styles.chevron} size={24} aria-hidden="true" />
    </span>
  );
}
