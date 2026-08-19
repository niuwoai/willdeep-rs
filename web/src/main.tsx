import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { ChakraProvider, defaultSystem } from "@chakra-ui/react";
import { App } from "./App";
import "./styles.css";
import "./routing.css";
import "./attachments.css";

createRoot(document.getElementById("root")!).render(
  <StrictMode><ChakraProvider value={defaultSystem}><App /></ChakraProvider></StrictMode>,
);
