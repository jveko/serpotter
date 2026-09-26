/**
 * Why a key is off, in the operator's words.
 *
 * `vendor_suspended` is a dead vendor account: toggling the key on will not
 * make it work, so it gets a warn chip that says an operator has to act.
 * `manual` is a key someone switched off deliberately. Anything else (a
 * reason a newer server may add) is shown verbatim rather than swallowed.
 */
export type DisabledReason = {
  /** CSS class suffix, "" for the neutral chip. */
  modifier: "" | "warn";
  label: string;
  title: string;
};

export function disabledReason(reason: string | null | undefined): DisabledReason | null {
  if (!reason) return null;
  if (reason === "vendor_suspended") {
    return {
      modifier: "warn",
      label: "vendor suspended",
      title: "Vendor account suspended — an operator has to re-enable this key.",
    };
  }
  if (reason === "manual") {
    return { modifier: "", label: "manual", title: "Disabled by an operator toggle." };
  }
  return { modifier: "", label: reason, title: `Disabled reason: ${reason}` };
}

/** Chip naming why an inactive key is off; null when there is no reason. */
export function DisabledReasonChip({ reason }: { reason: string | null | undefined }) {
  const chip = disabledReason(reason);
  if (!chip) return null;
  return (
    <span className={chip.modifier ? `chip chip--${chip.modifier}` : "chip"} title={chip.title}>
      {chip.label}
    </span>
  );
}
