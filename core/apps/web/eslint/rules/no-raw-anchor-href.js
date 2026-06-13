const ALLOWED_RAW_HREF_PREFIXES = ["/", "./", "../", "#", "?", "ctx://"];

const getAttribute = (node, name) =>
  node.attributes.find((attribute) => attribute.type === "JSXAttribute" && attribute.name?.name === name) ?? null;

const getStaticString = (valueNode) => {
  if (!valueNode) return null;
  if (valueNode.type === "Literal" && typeof valueNode.value === "string") {
    return valueNode.value;
  }
  if (valueNode.type !== "JSXExpressionContainer") return null;
  const expression = valueNode.expression;
  if (!expression) return null;
  if (expression.type === "Literal" && typeof expression.value === "string") {
    return expression.value;
  }
  if (
    expression.type === "TemplateLiteral"
    && expression.expressions.length === 0
    && expression.quasis.length === 1
  ) {
    return expression.quasis[0]?.value.cooked ?? null;
  }
  return null;
};

const isAllowedRawHref = (hrefValue) =>
  typeof hrefValue === "string" && ALLOWED_RAW_HREF_PREFIXES.some((prefix) => hrefValue.startsWith(prefix));

export default {
  meta: {
    type: "problem",
    docs: {
      description: "Disallow raw JSX anchors for navigable hrefs in favor of shared link components.",
    },
    schema: [],
    messages: {
      useSharedLink:
        "Use the shared link components instead of a raw <a href>. Use <ExternalLink> for external destinations, or keep raw anchors only for internal relative/ctx links.",
    },
  },
  create(context) {
    const filename =
      (typeof context.filename === "string" && context.filename)
      || (typeof context.getFilename === "function" ? context.getFilename() : "");
    if (filename.endsWith("src/components/ExternalLink.tsx")) {
      return {};
    }

    return {
      JSXOpeningElement(node) {
        if (node.name.type !== "JSXIdentifier" || node.name.name !== "a") return;
        if (getAttribute(node, "data-allow-raw-anchor")) return;
        if (getAttribute(node, "download")) return;

        const hrefAttribute = getAttribute(node, "href");
        if (!hrefAttribute) return;

        const hrefValue = getStaticString(hrefAttribute.value);
        if (isAllowedRawHref(hrefValue)) return;

        context.report({
          node,
          messageId: "useSharedLink",
        });
      },
    };
  },
};
