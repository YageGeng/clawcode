export type UiBootstrap = Readonly<{
  product: Readonly<{ name: string; slug: string }>;
  acpPath: string;
  defaultCwd: string;
  activeModel: Readonly<{ id: string; displayName: string }>;
}>;

export const UiBootstrapDecoder = {
  parse(value: unknown): UiBootstrap {
    if (typeof value !== "object" || value === null || Array.isArray(value)) {
      throw new Error("Bootstrap response must be an object");
    }
    const root = value as Record<string, unknown>;
    const productValue = root.product;
    const activeModelValue = root.activeModel;
    if (
      typeof productValue !== "object" ||
      productValue === null ||
      Array.isArray(productValue) ||
      typeof activeModelValue !== "object" ||
      activeModelValue === null ||
      Array.isArray(activeModelValue)
    ) {
      throw new Error("Bootstrap product and activeModel must be objects");
    }
    const product = productValue as Record<string, unknown>;
    const activeModel = activeModelValue as Record<string, unknown>;
    if (
      typeof product.name !== "string" ||
      typeof product.slug !== "string" ||
      typeof root.acpPath !== "string" ||
      typeof root.defaultCwd !== "string" ||
      typeof activeModel.id !== "string" ||
      typeof activeModel.displayName !== "string"
    ) {
      throw new Error("Bootstrap response contains invalid fields");
    }
    return {
      product: { name: product.name, slug: product.slug },
      acpPath: root.acpPath,
      defaultCwd: root.defaultCwd,
      activeModel: {
        id: activeModel.id,
        displayName: activeModel.displayName
      }
    };
  }
} as const;
