# hyperlane-explorer, pointed at this demo's local scraper instead of the
# hosted Hyperlane API.
#
# The explorer lives in its own repository, so it is cloned during the build
# rather than copied from a build context. Its `cardano` branch is what teaches
# it to render Cardano addresses, transactions and the 2003 domain.

FROM node:24-slim AS build

RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/*

ARG EXPLORER_REPO=https://github.com/equilibriumco/hyperlane-explorer.git
ARG EXPLORER_REF=cardano

WORKDIR /app
RUN git clone --depth 1 --branch "$EXPLORER_REF" "$EXPLORER_REPO" .

RUN corepack enable && pnpm install --frozen-lockfile

# Next.js inlines NEXT_PUBLIC_* at build time, so this cannot be supplied as a
# runtime environment variable. It is also read by the browser, not by this
# container, which is why it points at the host-published Hasura port rather
# than the `hasura` service name.
ARG NEXT_PUBLIC_API_URL=http://localhost:8080/v1/graphql
ENV NEXT_PUBLIC_API_URL=$NEXT_PUBLIC_API_URL

# The canonical registry has no cardanopreview entry at all, so without these
# the explorer cannot name the chain or recognise the wADA route.
ARG NEXT_PUBLIC_REGISTRY_URL=https://github.com/equilibriumco/hyperlane-registry
ARG NEXT_PUBLIC_REGISTRY_BRANCH=cardano
ENV NEXT_PUBLIC_REGISTRY_URL=$NEXT_PUBLIC_REGISTRY_URL
ENV NEXT_PUBLIC_REGISTRY_BRANCH=$NEXT_PUBLIC_REGISTRY_BRANCH

RUN pnpm build

EXPOSE 3000
CMD ["pnpm", "start"]
