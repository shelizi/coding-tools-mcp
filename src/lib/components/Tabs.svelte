<script lang="ts">
  import { t } from "$lib/i18n";

  interface TabItem {
    value: string;
    label: string;
  }

  interface Props {
    items: TabItem[];
    value: string;
    onchange: (value: string) => void;
    ariaLabel?: string;
    idPrefix?: string;
  }

  let { items, value, onchange, ariaLabel, idPrefix = "tabs" }: Props = $props();

  function itemId(itemValue: string): string {
    return `${idPrefix}-tab-${itemValue}`;
  }

  function panelId(itemValue: string): string {
    return `${idPrefix}-panel-${itemValue}`;
  }

  function focusTab(itemValue: string) {
    requestAnimationFrame(() => {
      document.getElementById(itemId(itemValue))?.focus();
    });
  }

  function handleKeydown(event: KeyboardEvent, index: number) {
    if (!items.length) return;

    let nextIndex: number | null = null;
    switch (event.key) {
      case "ArrowLeft":
        nextIndex = (index - 1 + items.length) % items.length;
        break;
      case "ArrowRight":
        nextIndex = (index + 1) % items.length;
        break;
      case "Home":
        nextIndex = 0;
        break;
      case "End":
        nextIndex = items.length - 1;
        break;
      default:
        return;
    }

    event.preventDefault();
    const nextValue = items[nextIndex].value;
    onchange(nextValue);
    focusTab(nextValue);
  }
</script>

<div
  class="tx-tabs"
  role="tablist"
  aria-label={ariaLabel ?? $t("Page sections")}
  aria-orientation="horizontal"
>
  {#each items as item, index (item.value)}
    <button
      type="button"
      role="tab"
      id={itemId(item.value)}
      aria-controls={panelId(item.value)}
      aria-selected={value === item.value}
      tabindex={value === item.value ? 0 : -1}
      class="tx-tab"
      class:active={value === item.value}
      onclick={() => onchange(item.value)}
      onkeydown={(event) => handleKeydown(event, index)}
    >
      {item.label}
    </button>
  {/each}
</div>
