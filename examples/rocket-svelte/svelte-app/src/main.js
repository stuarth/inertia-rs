import { createInertiaApp } from '@inertiajs/svelte'
import './global.css'

const pages = import.meta.glob('./Pages/**/*.svelte', { eager: true })

createInertiaApp({
  resolve: name => pages[`./Pages/${name}.svelte`],
})
