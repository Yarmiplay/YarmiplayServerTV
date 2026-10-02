import { untrack } from "svelte";
import { clone } from "./api";

/**
 * Editable copy of part of the saved settings. Follows the saved value while
 * there are no local edits; keeps the edits otherwise.
 */
export function useDraft<T>(source: () => T) {
  let base = $state(clone(source()));
  let draft = $state(clone(source()));
  const dirty = $derived(JSON.stringify(draft) !== JSON.stringify(base));

  $effect(() => {
    const incoming = source();
    const text = JSON.stringify(incoming);
    untrack(() => {
      if (text !== JSON.stringify(base)) {
        if (!dirty) draft = clone(incoming);
        base = clone(incoming);
      }
    });
  });

  return {
    get draft() {
      return draft;
    },
    get dirty() {
      return dirty;
    },
    reset() {
      draft = clone(base);
    },
  };
}
