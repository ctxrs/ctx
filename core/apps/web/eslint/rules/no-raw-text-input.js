const ALLOWED_RAW_INPUT_TYPES = new Set([
  "button",
  "checkbox",
  "color",
  "file",
  "hidden",
  "image",
  "radio",
  "range",
  "reset",
  "submit",
]);

const getAttribute = (node, name) =>
  node.attributes.find((attribute) => attribute.type === "JSXAttribute" && attribute.name?.name === name) ?? null;

const collectStaticStringsFromExpression = (expression) => {
  if (!expression) return [];
  if (expression.type === "Literal" && typeof expression.value === "string") {
    return [expression.value];
  }
  if (
    expression.type === "TemplateLiteral"
    && expression.expressions.length === 0
    && expression.quasis.length === 1
  ) {
    return expression.quasis[0]?.value.cooked ? [expression.quasis[0].value.cooked] : [];
  }
  if (expression.type === "ConditionalExpression") {
    const consequent = collectStaticStringsFromExpression(expression.consequent);
    const alternate = collectStaticStringsFromExpression(expression.alternate);
    return [...consequent, ...alternate];
  }
  return [];
};

const getStaticStrings = (valueNode) => {
  if (!valueNode) return [];
  if (valueNode.type === "Literal" && typeof valueNode.value === "string") {
    return [valueNode.value];
  }
  if (valueNode.type !== "JSXExpressionContainer") return [];
  return collectStaticStringsFromExpression(valueNode.expression);
};

const isEnabledEscapeHatch = (attribute) => {
  if (!attribute) return false;
  if (attribute.value == null) return true;
  if (attribute.value.type !== "JSXExpressionContainer") return false;
  return attribute.value.expression.type === "Literal" && attribute.value.expression.value === true;
};

const isTextInputImplementationFile = (filename) =>
  /(^|[\\/])src[\\/]components[\\/]ui[\\/]text-input\.tsx$/.test(filename);

export default {
  meta: {
    type: "problem",
    docs: {
      description: "Disallow raw text-entry inputs and textareas in favor of shared text input primitives.",
    },
    schema: [],
    messages: {
      useSharedTextInput:
        "Use the shared <TextInput> / <Textarea> primitives instead of raw text-entry elements so IDE-style text behavior stays consistent.",
    },
  },
  create(context) {
    const filename =
      (typeof context.filename === "string" && context.filename)
      || (typeof context.getFilename === "function" ? context.getFilename() : "");
    if (isTextInputImplementationFile(filename)) {
      return {};
    }

    return {
      JSXOpeningElement(node) {
        if (node.name.type !== "JSXIdentifier") return;
        if (isEnabledEscapeHatch(getAttribute(node, "data-allow-raw-text-input"))) return;

        if (node.name.name === "textarea") {
          context.report({
            node,
            messageId: "useSharedTextInput",
          });
          return;
        }

        if (node.name.name !== "input") return;

        const typeAttribute = getAttribute(node, "type");
        const typeValues = getStaticStrings(typeAttribute?.value).map((value) => value.toLowerCase());
        if (typeValues.length > 0 && typeValues.every((value) => ALLOWED_RAW_INPUT_TYPES.has(value))) return;

        context.report({
          node,
          messageId: "useSharedTextInput",
        });
      },
    };
  },
};
