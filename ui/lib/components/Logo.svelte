<script lang="ts">
  let { size = 24 }: { size?: number } = $props();
  const id = `patchbay-logo-${Math.random().toString(36).slice(2, 8)}`;
</script>

<svg width={size} height={size} viewBox="64 64 896 896" aria-hidden="true">
  <defs>
    <linearGradient {id} x1="0.2" y1="0" x2="0.8" y2="1">
      <stop offset="0" stop-color="#334155" />
      <stop offset="1" stop-color="#0f172a" />
    </linearGradient>
  </defs>
  <rect x="64" y="64" width="896" height="896" rx="200" fill="url(#{id})" />
  <g fill="#020617" fill-opacity="0.45" stroke="#e0e7ff" stroke-width="26">
    <circle cx="292" cy="372" r="62" /><circle cx="512" cy="372" r="62" /><circle cx="732" cy="372" r="62" />
    <circle cx="292" cy="652" r="62" /><circle cx="512" cy="652" r="62" /><circle cx="732" cy="652" r="62" />
  </g>
  <g fill="none" stroke-linecap="round">
    <path d="M512 372 C512 560 292 464 292 652" stroke="#5eead4" stroke-width="60" />
    <path d="M292 372 C292 600 732 424 732 652" stroke="#020617" stroke-opacity="0.3" stroke-width="100" />
    <path d="M292 372 C292 600 732 424 732 652" stroke="#fbbf24" stroke-width="60" />
  </g>
  <g>
    <circle cx="512" cy="372" r="49" fill="#5eead4" /><circle cx="292" cy="652" r="49" fill="#5eead4" />
    <circle cx="292" cy="372" r="49" fill="#fbbf24" /><circle cx="732" cy="652" r="49" fill="#fbbf24" />
  </g>
</svg>
