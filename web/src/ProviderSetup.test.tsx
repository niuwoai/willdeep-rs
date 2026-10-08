import { renderToStaticMarkup } from "react-dom/server";
import { ChakraProvider, defaultSystem } from "@chakra-ui/react";
import { expect, it } from "vitest";
import { ProviderSetup } from "./ProviderSetup";
import { messages } from "./i18n";

it("guides an unapproved fresh plugin and explains the terminal fallback", () => {
  const status = { ready: false, ruby_available: false, config_path: "/tmp/test/config.toml" };
  const html = renderToStaticMarkup(<ChakraProvider value={defaultSystem}>
    <ProviderSetup status={status} plugin={undefined} messages={messages["en"]}
      onOpen={() => undefined} onCheck={async () => status} onDone={() => undefined} />
  </ChakraProvider>);
  expect(html).toContain("Review willdeep-config permissions");
  expect(html).toContain("Open plugin center");
  expect(html).toContain("willdeep --onboarding");
  expect(html).toContain("/tmp/test/config.toml");
});
