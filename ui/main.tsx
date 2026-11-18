import { render } from "preact";
import { App } from "./app";
import "@fontsource-variable/inter";
import "@fontsource-variable/noto-sans-sc";
import "./styles.css";

render(<App />, document.getElementById("app")!);
