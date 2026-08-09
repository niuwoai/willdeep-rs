import{StrictMode}from"react";import{createRoot}from"react-dom/client";import{ChakraProvider,defaultSystem}from"@chakra-ui/react";import{App}from"./App";import{zhCN as t}from"./i18n";import"./styles.css";
document.title=t.documentTitle;
createRoot(document.getElementById("root")!).render(<StrictMode><ChakraProvider value={defaultSystem}><App/></ChakraProvider></StrictMode>);
