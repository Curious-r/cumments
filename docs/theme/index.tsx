import { Banner, Layout as BasicLayout } from "@rspress/core/theme-original";

const docsVersion = process.env.CUMMENTS_DOCS_VERSION;

const message =
  docsVersion === ""
    ? null
    : docsVersion === "main"
      ? "Unreleased development documentation (main branch)."
      : `Documentation for Cumments ${docsVersion}.`;

const Layout = () => (
  <BasicLayout
    beforeNav={
      message ? (
        <Banner href="/" message={message} storage={false} />
      ) : undefined
    }
  />
);

export * from "@rspress/core/theme-original";
export { Layout };
