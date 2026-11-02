<script>
  import { onMount } from 'svelte';

  let { value } = $props();
  let now = $state(Date.now());
  const date = $derived(new Date(typeof value === 'number' ? value * 1000 : value));
  const valid = $derived(value != null && Number.isFinite(date.getTime()));
  const label = $derived.by(() => {
    const seconds = Math.max(0, Math.floor((now - date.getTime()) / 1000));
    if (seconds < 60) return 'just now';
    const minutes = Math.floor(seconds / 60);
    if (minutes < 60) return `${minutes} ${minutes === 1 ? 'minute' : 'minutes'} ago`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `${hours} ${hours === 1 ? 'hour' : 'hours'} ago`;
    const days = Math.floor(hours / 24);
    return `${days} ${days === 1 ? 'day' : 'days'} ago`;
  });

  onMount(() => {
    const timer = setInterval(() => now = Date.now(), 30_000);
    return () => clearInterval(timer);
  });
</script>

{#if valid}<time datetime={date.toISOString()} title={date.toLocaleString()}>{label}</time>{:else}time unavailable{/if}
