<script>
  let { text = '', label, disabled = false, children } = $props();
  const tooltipId = $props.id();
  let trigger = $state();
  let tooltip = $state();
  let open = $state(false);

  function show() {
    if (!text || !tooltip) return;
    tooltip.showPopover();
    open = true;
    position();
  }

  function position() {
    const anchor = trigger.getBoundingClientRect();
    const bounds = tooltip.getBoundingClientRect();
    const left = Math.max(8, Math.min(anchor.left, window.innerWidth - bounds.width - 8));
    const top = anchor.top >= bounds.height + 8 ? anchor.top - bounds.height
      : Math.min(anchor.bottom, window.innerHeight - bounds.height - 8);
    tooltip.style.left = `${left}px`;
    tooltip.style.top = `${Math.max(8, top)}px`;
  }

  function hide() {
    tooltip?.hidePopover();
    open = false;
  }

  function trackPosition() {
    if (!open || !tooltip) return;
    const anchor = trigger.getBoundingClientRect();
    if (anchor.bottom < 0 || anchor.top > window.innerHeight) hide();
    else position();
  }

  function leave(event) {
    if (!trigger.contains(event.relatedTarget) && !trigger.contains(document.activeElement)) hide();
  }

  function dismiss(event) {
    if (event.key === 'Escape') hide();
  }

  $effect(() => {
    if (!text) hide();
    else if (open && tooltip) position();
  });
</script>

<svelte:window onscroll={trackPosition} onresize={trackPosition} onkeydown={dismiss} />

<!-- A disabled button cannot receive focus; its wrapper exposes the explanation to keyboard users. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<span class="button-help" bind:this={trigger} role="group" aria-label={label}
  aria-disabled={disabled || undefined} aria-describedby={text ? tooltipId : undefined}
  tabindex={text && disabled ? 0 : undefined}
  onpointerenter={show} onpointerleave={leave} onfocusin={show}
  onfocusout={(event) => { if (!trigger.contains(event.relatedTarget)) hide(); }}>
  {@render children(text ? tooltipId : undefined)}
  {#if text}
    <span class="button-tooltip" bind:this={tooltip} id={tooltipId} role="tooltip" popover="manual">{text}</span>
  {/if}
</span>

<style>
  .button-help {
    position: relative;
    display: inline-flex;
    align-self: flex-start;
    border-radius: 24px;
  }

  /* Disabled native buttons suppress pointer events; let the wrapper receive them. */
  .button-help[aria-disabled="true"]::before {
    content: '';
    position: absolute;
    inset: 0;
    z-index: 1;
    border-radius: inherit;
  }

  .button-help:focus-visible {
    outline: 2px solid var(--md-sys-color-primary);
    outline-offset: 3px;
  }

  .button-tooltip {
    position: fixed;
    inset: auto;
    margin: 0;
    max-width: min(320px, calc(100vw - 16px));
    padding: 10px 12px;
    border: 0;
    border-radius: 8px;
    background: #322f35;
    color: #fff;
    font-size: 0.85rem;
    line-height: 1.4;
    overflow-wrap: anywhere;
    box-shadow: 0 3px 12px rgb(0 0 0 / 20%);
  }
</style>
