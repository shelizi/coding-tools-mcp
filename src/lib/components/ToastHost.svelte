<script lang="ts">
  import { fly } from "svelte/transition";
  import AlertTriangle from "@lucide/svelte/icons/alert-triangle";
  import CheckCircle2 from "@lucide/svelte/icons/check-circle-2";
  import Info from "@lucide/svelte/icons/info";
  import X from "@lucide/svelte/icons/x";
  import XCircle from "@lucide/svelte/icons/x-circle";
  import { t } from "$lib/i18n";
  import { dismissToast, toasts, type ToastKind } from "$lib/stores/toast";

  const icons: Record<ToastKind, typeof Info> = {
    info: Info,
    success: CheckCircle2,
    warning: AlertTriangle,
    error: XCircle,
  };
</script>

<div class="tx-toast-host" aria-live="polite" aria-atomic="false">
  {#each $toasts as toast (toast.id)}
  {@const Icon = icons[toast.kind]}
    <div
      class="tx-toast tx-toast--{toast.kind}"
      role="status"
      transition:fly={{ x: 24, duration: 220 }}
    >
      <div class="tx-toast__icon" aria-hidden="true">
        <Icon size={18} strokeWidth={2.25} />
      </div>
      <div class="tx-toast__body">
        {#if toast.title}
          <p class="tx-toast__title">{toast.title}</p>
        {/if}
        <p class="tx-toast__message">{toast.message}</p>
      </div>
      <button
        type="button"
        class="tx-toast__close"
        aria-label={$t("Close")}
        onclick={() => dismissToast(toast.id)}
      >
        <X size={14} strokeWidth={2.25} />
      </button>
    </div>
  {/each}
</div>
