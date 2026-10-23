const { parse } = require("./vendor/acorn.cjs");

// Slice only the entrypoint. Selected dependencies run with normal Node semantics.
function selectExports(source, filters) {
  const ast = parse(source, { ecmaVersion: "latest", sourceType: "module", allowHashBang: true });
  const units = [];
  const statements = [];
  const exports = new Map();
  const bindings = new Map();

  function unit(node, names, render, exported = []) {
    const item = { node, render, exported, keep: false };
    units.push(item);
    for (const name of names) {
      const declarations = bindings.get(name) || [];
      declarations.push(item);
      bindings.set(name, declarations);
    }
    for (const name of exported) exports.set(name, item);
    return item;
  }

  function declaration(node, exported, prefix = "") {
    const declarations = node.type === "VariableDeclaration" ? node.declarations : [node];
    return declarations.map((decl) => {
      const names = bindingNames(decl.id);
      const text = node.type === "VariableDeclaration"
        ? `${node.kind} ${source.slice(decl.start, decl.end)};`
        : source.slice(decl.start, decl.end);
      return unit(decl, names, (selected) => `${selected ? prefix || "export " : ""}${text}`,
        exported ? (prefix ? ["default"] : names) : []);
    });
  }

  for (const node of ast.body) {
    let items;
    if (node.type === "ImportDeclaration") {
      if (!node.specifiers.length) {
        items = [unit(null, [], () => source.slice(node.start, node.end))];
        items[0].keep = true;
      } else {
        items = node.specifiers.map((specifier) => {
          const text = source.slice(specifier.start, specifier.end);
          const clause = specifier.type === "ImportSpecifier" ? `{ ${text} }` : text;
          return unit(null, [specifier.local.name], () =>
            `import ${clause} from ${source.slice(node.source.start, node.end)};`);
        });
      }
    } else if (node.type === "ExportAllDeclaration") {
      if (!node.exported) {
        throw new Error("--selective-load does not support export *; use explicit named exports");
      }
      items = [unit(null, [], () => source.slice(node.start, node.end), [exportName(node.exported)])];
    } else if (node.type === "ExportNamedDeclaration") {
      if (node.declaration) {
        items = declaration(node.declaration, true);
      } else {
        items = node.specifiers.map((specifier) => unit(node.source ? null : specifier.local, [],
          () => `export { ${source.slice(specifier.start, specifier.end)} }${node.source
            ? ` from ${source.slice(node.source.start, node.end)};` : ";"}`,
          [exportName(specifier.exported)]));
      }
    } else if (node.type === "ExportDefaultDeclaration") {
      const decl = node.declaration;
      if ((decl.type === "FunctionDeclaration" || decl.type === "ClassDeclaration") && decl.id) {
        items = declaration(decl, true, "export default ");
      } else {
        items = [unit(decl, [], () => source.slice(node.start, node.end), ["default"])];
      }
    } else if (["VariableDeclaration", "FunctionDeclaration", "ClassDeclaration"].includes(node.type)) {
      items = declaration(node, false);
    } else {
      // Keep setup statements (including their dependencies) in their original order.
      items = [unit(node, [], () => source.slice(node.start, node.end))];
      items[0].keep = true;
    }
    statements.push({ node, items });
  }

  const selected = new Set(filters.map((filter) => filter.split(/[./]/)[0]));
  if (!selected.size) throw new Error("--selective-load requires at least one --filter");
  for (const name of selected) {
    if (!exports.has(name)) {
      throw new Error(`--selective-load cannot find export '${name}'; filters must use static export paths, not runtime function names`);
    }
    exports.get(name).keep = true;
  }

  // Over-approximate references: shadowed identifiers may retain extra bindings,
  // but must never cause a dependency to be removed.
  const queue = units.filter((item) => item.keep);
  for (let i = 0; i < queue.length; i++) {
    identifiers(queue[i].node, (name) => {
      if (name === "eval") {
        throw new Error("--selective-load does not support eval in retained entrypoint code");
      }
      for (const dependency of bindings.get(name) || []) {
        if (!dependency.keep) {
          dependency.keep = true;
          queue.push(dependency);
        }
      }
    });
  }

  let result = "";
  let end = 0;
  for (const { node, items } of statements) {
    result += source.slice(end, node.start);
    const replacement = items.filter((item) => item.keep)
      .map((item) => item.render(item.exported.some((name) => selected.has(name))))
      .join(" ");
    result += replacement;
    // Keep subsequent statement line numbers stable when deleting declarations.
    const removedLines = source.slice(node.start, node.end).split("\n").length
      - replacement.split("\n").length;
    result += "\n".repeat(Math.max(0, removedLines));
    end = node.end;
  }
  return result + source.slice(end);
}

function exportName(node) {
  return node.name ?? node.value;
}

function bindingNames(node) {
  if (!node) return [];
  switch (node.type) {
    case "Identifier": return [node.name];
    case "RestElement": return bindingNames(node.argument);
    case "AssignmentPattern": return bindingNames(node.left);
    case "ArrayPattern": return node.elements.flatMap(bindingNames);
    case "ObjectPattern": return node.properties.flatMap((prop) =>
      bindingNames(prop.type === "RestElement" ? prop.argument : prop.value));
    default: return [];
  }
}

function identifiers(node, visit) {
  if (!node || typeof node !== "object") return;
  if (node.type === "Identifier") visit(node.name);
  for (const [key, value] of Object.entries(node)) {
    if ((key === "key" || key === "property") && !node.computed && !node.shorthand) continue;
    if (Array.isArray(value)) value.forEach((child) => identifiers(child, visit));
    else if (value && typeof value === "object") identifiers(value, visit);
  }
}

module.exports = { selectExports };
