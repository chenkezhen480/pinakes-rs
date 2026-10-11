<!-- The one place the console inserts HTML: a page body the service rendered and made safe
     (`wiki::render`: raw HTML shown as text, only web and mail links, no images). Wiki links
     are console routes, followed in place. -->
<script lang="ts">
  let { html }: { html: string } = $props();
</script>

<div class="prose">{@html html}</div>
