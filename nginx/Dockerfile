# Build the SPA, then serve it as static files.
#
# Node appears only in this build stage: the runtime image is nginx plus a
# directory of assets, so no Node process exists in production. This replaces
# the previous arrangement, where a Next.js server ran alongside nginx and
# handed it static output through a shared named volume.
#
# The build context is the repository root (see docker-compose.yml) because
# this image now needs frontend/ as well as nginx/.
FROM node:24-alpine AS builder
WORKDIR /app

# Dependencies first, so a source-only change reuses this layer.
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci

COPY frontend/ ./
RUN npm run build

FROM nginx:alpine
RUN rm /etc/nginx/conf.d/default.conf
COPY nginx/nginx.conf /etc/nginx/nginx.conf
COPY nginx/security-headers.conf /etc/nginx/security-headers.conf
COPY --from=builder /app/dist /usr/share/nginx/html

HEALTHCHECK --interval=10s --timeout=3s --retries=3 \
    CMD wget -q --spider http://127.0.0.1/nginx-health || exit 1
