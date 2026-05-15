<script>
  import { router } from '@inertiajs/svelte'

  let {
    appName = 'Inertia Example',
    auth = {},
    name = 'world',
    message = '',
    stats,
    debug,
  } = $props()

  let loading = $state(null)

  const formatGeneratedAt = seconds => (
    new Date(seconds * 1000).toLocaleTimeString()
  )

  const reloadProps = (only, key) => {
    loading = key
    router.reload({
      only,
      onFinish: () => {
        loading = null
      },
    })
  }

  const loadDebug = () => {
    reloadProps(['debug'], 'debug')
  }

  const loadStats = () => {
    reloadProps(['stats'], 'stats')
  }

  const loadBoth = () => {
    reloadProps(['stats', 'debug'], 'both')
  }
</script>

<main>
  <header class="masthead">
    <p class="eyebrow">{appName}</p>
    <h1>Hello {name}</h1>
    {#if message}
      <p>{message}</p>
    {/if}
  </header>

  <section class="summary" aria-label="Shared props">
    <span>Signed in as</span>
    <strong>{auth?.user?.name ?? 'Guest'}</strong>
    <small>{auth?.user?.role ?? 'No role'}</small>
  </section>

  <section class="props" aria-label="Route props">
    <article>
      <span class="label">Deferred stats</span>
      {#if stats}
        <strong>{stats.adapter}</strong>
        <span>Generated at {formatGeneratedAt(stats.generatedAt)}</span>
      {:else}
        <span class="muted">Waiting for a partial reload</span>
      {/if}
    </article>

    <article>
      <span class="label">Optional debug</span>
      {#if debug}
        <strong>{debug.loadedBy}</strong>
        <span>Partial reload: {debug.partialReload ? 'yes' : 'no'}</span>
      {:else}
        <span class="muted">Not requested yet</span>
      {/if}
    </article>
  </section>

  <div class="actions">
    <button type="button" disabled={loading !== null} onclick={loadStats}>
      {loading === 'stats' ? 'Reloading stats' : 'Reload stats'}
    </button>
    <button type="button" disabled={loading !== null} onclick={loadDebug}>
      {loading === 'debug' ? 'Loading debug' : 'Load debug'}
    </button>
    <button type="button" disabled={loading !== null} onclick={loadBoth}>
      {loading === 'both' ? 'Loading both' : 'Load both'}
    </button>
  </div>
</main>
