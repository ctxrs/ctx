function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

export function normalizeSearchText(value: string): string {
  return value
    .toLowerCase()
    .replace(/['’]/g, '')
    .replace(/[^a-z0-9]+/g, ' ')
    .trim();
}

export function tokenizeSearchQuery(query: string): string[] {
  return normalizeSearchText(query)
    .split(/\s+/)
    .filter((token) => token.length > 0);
}

export function expandSearchTokenVariants(token: string): string[] {
  const variants = new Set<string>([token]);

  if (token.endsWith('ies') && token.length > 4) {
    variants.add(`${token.slice(0, -3)}y`);
  }

  if (token.endsWith('es') && token.length > 4) {
    variants.add(token.slice(0, -2));
  }

  if (token.endsWith('s') && token.length > 3) {
    variants.add(token.slice(0, -1));
  }

  return [...variants].filter((value) => value.length > 0);
}

export function getSearchTextMatchTier(value: string, query: string): number {
  const normalizedValue = normalizeSearchText(value);
  const normalizedQuery = normalizeSearchText(query);
  if (normalizedValue.length === 0 || normalizedQuery.length === 0) {
    return 0;
  }

  let tier = 0;

  for (const variant of expandSearchTokenVariants(normalizedQuery)) {
    if (normalizedValue === variant) {
      return 4;
    }

    if (normalizedValue.startsWith(variant)) {
      tier = Math.max(tier, 3);
      continue;
    }

    if (new RegExp(`(^|[ /_-])${escapeRegExp(variant)}(?=$|[ /_-])`).test(normalizedValue)) {
      tier = Math.max(tier, 2);
      continue;
    }

    if (normalizedValue.includes(variant)) {
      tier = Math.max(tier, 1);
    }
  }

  return tier;
}
