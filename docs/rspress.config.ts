import { defineConfig } from "@rspress/core";

const docsVersion = process.env.CUMMENTS_DOCS_VERSION ?? "";

export default defineConfig({
  root: import.meta.dirname,
  themeDir: `${import.meta.dirname}/theme`,
  outDir: `${import.meta.dirname}/../site`,
  title: "Cumments",
  description:
    "Matrix-backed comments for ordinary websites, with visitor identity and moderation.",
  lang: "en",
  siteOrigin: "https://cumments.curious.host",
  route: {
    cleanUrls: true,
    localeRedirect: "never",
    // The config lives with the content for a self-contained docs workspace,
    // but only Markdown files are public routes.
    exclude: ["rspress.config.*", "theme/**"],
  },
  markdown: {
    link: {
      checkDeadLinks: {
        // The contract is a public asset copied from docs/public, not a route.
        excludes: ["/openapi.yaml"],
      },
      checkAnchors: true,
    },
  },
  builderConfig: {
    output: {
      cleanDistPath: false,
    },
    source: {
      define: {
        "process.env.CUMMENTS_DOCS_VERSION": JSON.stringify(docsVersion),
      },
    },
  },
  themeConfig: {
    socialLinks: [
      {
        icon: "github",
        mode: "link",
        content: "https://github.com/curious-r/cumments",
      },
    ],
    nav: [
      {
        text: "Getting started",
        link: "/",
        items: [
          { text: "Home", link: "/" },
          { text: "Quick start", link: "/quick-start" },
        ],
      },
      {
        text: "Concepts",
        link: "/architecture",
        items: [
          { text: "Architecture", link: "/architecture" },
          { text: "Data model", link: "/data-model" },
          {
            text: "Site trust",
            items: [
              { text: "Overview", link: "/site-trust" },
              { text: "Verification walkthrough", link: "/site-verification" },
            ],
          },
          { text: "Site governance", link: "/site-governance" },
        ],
      },
      {
        text: "Reference",
        link: "/api/index",
        items: [
          { text: "API overview", link: "/api/index" },
          { text: "Configuration", link: "/configuration" },
          { text: "CLI", link: "/cli" },
          { text: "Problem types", link: "/problems/index" },
        ],
      },
      {
        text: "Development",
        link: "/development",
        items: [
          { text: "Development", link: "/development" },
          { text: "Demo frontend", link: "/demo" },
        ],
      },
    ],
    sidebar: {
      "/": [
        {
          text: "Getting started",
          link: "/",
          items: [
            { text: "Home", link: "/" },
            { text: "Quick start", link: "/quick-start" },
          ],
        },
        {
          text: "Concepts",
          link: "/architecture",
          items: [
            { text: "Architecture", link: "/architecture" },
            { text: "Data model", link: "/data-model" },
            {
              text: "Site trust",
              items: [
                { text: "Overview", link: "/site-trust" },
                {
                  text: "Verification walkthrough",
                  link: "/site-verification",
                },
              ],
            },
            { text: "Site governance", link: "/site-governance" },
          ],
        },
        {
          text: "API",
          link: "/api/index",
          items: [
            { text: "Overview", link: "/api/index" },
            { text: "Comments", link: "/api/comments" },
            { text: "Sites", link: "/api/sites" },
            { text: "Governance", link: "/api/governance" },
            { text: "Operator", link: "/api/operator" },
            { text: "Media", link: "/api/media" },
            { text: "Visitors", link: "/api/visitors" },
            { text: "OpenAPI 3.2 status", link: "/openapi-3-2" },
          ],
        },
        {
          text: "Reference",
          items: [
            { text: "Configuration", link: "/configuration" },
            { text: "CLI", link: "/cli" },
            { text: "Problem types", link: "/problems/index" },
          ],
        },
        {
          text: "Development",
          link: "/development",
          items: [
            { text: "Development", link: "/development" },
            { text: "Demo frontend", link: "/demo" },
          ],
        },
      ],
    },
  },
});
