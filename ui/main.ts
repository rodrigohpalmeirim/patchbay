import "./app.css";
// Applies the persisted theme before the first render.
import "./lib/stores/settings.svelte";
import { mount } from "svelte";
import App from "./App.svelte";
import { customTitlebar } from "./lib/util/platform";

document.documentElement.classList.toggle("custom-titlebar", customTitlebar);

const app = mount(App, { target: document.getElementById("app")! });

export default app;
