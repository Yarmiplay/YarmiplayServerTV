<script lang="ts">
  let {
    checked,
    label,
    hint = "",
    disabled = false,
    onchange,
  }: { checked: boolean; label: string; hint?: string; disabled?: boolean; onchange: (v: boolean) => void } = $props();
</script>

<label class="toggle" class:disabled>
  <span class="text">
    <span class="label">{label}</span>
    {#if hint}<span class="hint">{hint}</span>{/if}
  </span>
  <input type="checkbox" role="switch" {checked} {disabled} onchange={(e) => onchange(e.currentTarget.checked)} />
  <span class="track" aria-hidden="true"><span class="thumb"></span></span>
</label>

<style>
  .toggle {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    cursor: pointer;
    padding: 6px 0;
  }
  .toggle.disabled {
    opacity: 0.5;
    cursor: default;
  }
  .text {
    display: flex;
    flex-direction: column;
  }
  .label {
    font-weight: 600;
  }
  .hint {
    color: var(--muted);
    font-size: 0.88em;
  }
  input {
    position: absolute;
    opacity: 0;
    width: 0;
    height: 0;
  }
  .track {
    flex: none;
    width: 42px;
    height: 24px;
    border-radius: 99px;
    background: var(--card-high);
    border: 1px solid var(--line);
    position: relative;
    transition: background 0.15s;
  }
  .thumb {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--muted);
    transition: transform 0.15s, background 0.15s;
  }
  input:checked + .track {
    background: var(--accent);
    border-color: var(--accent);
  }
  input:checked + .track .thumb {
    transform: translateX(18px);
    background: var(--on-accent);
  }
  input:focus-visible + .track {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
