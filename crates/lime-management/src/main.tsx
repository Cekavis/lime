import React from "react";
import { createRoot } from "react-dom/client";
import { setNonce } from "get-nonce";
import { App } from "./app/app";
import { ManagementProvider } from "./app/management-context";
import { ThemeProvider } from "./app/theme-context";
import "./style.css";

// Tauri replaces this token with a per-response nonce and adds it to style-src.
// Radix's scroll lock can then inject its stylesheet without relaxing the CSP.
const styleNonce = document.querySelector<HTMLMetaElement>('meta[name="lime-style-nonce"]')?.content;
if (styleNonce && !styleNonce.startsWith("__TAURI_")) setNonce(styleNonce);

const root = document.getElementById("app");
if (!root) throw new Error("Lime UI mount point is missing");
createRoot(root).render(<React.StrictMode><ThemeProvider><ManagementProvider><App /></ManagementProvider></ThemeProvider></React.StrictMode>);
