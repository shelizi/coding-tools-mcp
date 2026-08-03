<script lang="ts">
  import Languages from "@lucide/svelte/icons/languages";
  import ChevronDown from "@lucide/svelte/icons/chevron-down";
  import { LOCALE_OPTIONS, locale, setLocale, t, type Locale } from "$lib/i18n";

  function handleChange(event: Event) {
    setLocale((event.currentTarget as HTMLSelectElement).value as Locale);
  }
</script>

<label
  class="inline-flex h-9 items-center gap-1.5 rounded-[10px] border border-white/10 bg-white/5 px-2 text-[#c5d0ea] shadow-sm transition-[background-color,border-color,box-shadow] hover:border-white/20 hover:bg-white/10 focus-within:border-[#5b8aff]/70 focus-within:bg-white/10 focus-within:ring-2 focus-within:ring-[#5b8aff]/20"
>
  <Languages size={15} aria-hidden="true" />
  <span class="sr-only">{$t("Language")}</span>
  <select
    class="language-select min-w-[5.25rem] max-w-24 cursor-pointer appearance-none border-0 bg-transparent py-1 pr-1 text-xs font-semibold outline-none"
    aria-label={$t("Language")}
    value={$locale}
    onchange={handleChange}
  >
    {#each LOCALE_OPTIONS as option}
      <option value={option.value}>{option.label}</option>
    {/each}
  </select>
  <ChevronDown class="pointer-events-none shrink-0 text-[#8ea0c4]" size={13} aria-hidden="true" />
</label>

<style>
  .language-select {
    color-scheme: dark;
    color: #e6eefc;
  }

  .language-select option {
    background-color: #17243a;
    color: #eef4ff;
  }
</style>
