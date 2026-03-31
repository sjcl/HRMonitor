FROM node:24-alpine AS builder

ARG REACT_PROFILING=false

WORKDIR /app
COPY package.json package-lock.json* ./
RUN npm install

COPY . .
ENV REACT_PROFILING=${REACT_PROFILING}
RUN npm run build

FROM node:24-alpine

WORKDIR /app

COPY --from=builder /app/.next/standalone ./
COPY --from=builder /app/.next/static ./.next/static
COPY --from=builder /app/public ./public
COPY entrypoint.sh ./
RUN sed -i 's/\r$//' entrypoint.sh && chmod +x entrypoint.sh

EXPOSE 3000

CMD ["./entrypoint.sh"]
