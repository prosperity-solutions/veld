# Veld Scenarios Guide

This is a collection of real-world configuration patterns for Veld. Each scenario presents a practical problem, a complete (or near-complete) `veld.json` config, and a brief explanation of what happens at runtime. These patterns are composable -- combine them freely to match your actual project structure.

For the full field reference, see [configuration.md](./configuration.md).

---

## Table of Contents

1. [Fullstack Monorepo](#1-fullstack-monorepo)
2. [Cross-Referencing URLs Without Cyclic Dependencies](#2-cross-referencing-urls-without-cyclic-dependencies)
3. [Multiple Databases (Postgres + Redis)](#3-multiple-databases-postgres--redis)
4. [Local vs. Staging Backend with Variants](#4-local-vs-staging-backend-with-variants)
5. [Database Cloning with Idempotency](#5-database-cloning-with-idempotency)
6. [Microservices with Complex Dependency Graph](#6-microservices-with-complex-dependency-graph)
7. [Monorepo with Shared Setup Steps](#7-monorepo-with-shared-setup-steps)
8. [Docker Infrastructure with Synthetic Outputs](#8-docker-infrastructure-with-synthetic-outputs)
9. [Feature Branch Isolation](#9-feature-branch-isolation)
10. [Multi-Developer / Team URL Isolation](#10-multi-developer--team-url-isolation)
11. [Sensitive Credentials](#11-sensitive-credentials)
12. [Custom Apex Domains](#12-custom-apex-domains)
13. [Hybrid Local/Remote Services](#13-hybrid-localremote-services)
14. [Build Step Before Server](#14-build-step-before-server)
15. [Worker Processes Alongside API Servers](#15-worker-processes-alongside-api-servers)
16. [Polyglot Monorepo (Mixed Languages)](#16-polyglot-monorepo-mixed-languages)
17. [End-to-End Test Runner](#17-end-to-end-test-runner)

---

## 1. Fullstack Monorepo

**When to use:** You have a typical monorepo with a frontend app, a backend API, and a database. All three run locally during development.

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "shopfront",
  "url_template": "{service}.{branch ?? run}.shopfront.localhost",

  "nodes": {
    "database": {
      "variants": {
        "docker": {
          "type": "start_server",
          "command": "docker run --rm --name veld-pg-${veld.run} -e POSTGRES_PASSWORD=veld -e POSTGRES_DB=shopfront -p ${veld.port}:5432 postgres:16",
          "on_stop": "docker stop veld-pg-${veld.run}",
          "health_check": { "type": "port", "timeout_seconds": 30 },
          "outputs": {
            "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/shopfront"
          }
        }
      }
    },

    "backend": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @shopfront/api dev --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" },
          "depends_on": { "database": "docker" },
          "env": {
            "DATABASE_URL": "${nodes.database.DATABASE_URL}",
            "NODE_ENV": "development"
          }
        }
      }
    },

    "frontend": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @shopfront/web dev",
          "health_check": { "type": "http", "path": "/" },
          "depends_on": { "backend": "local" },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_API_URL": "${nodes.backend.url}"
          }
        }
      }
    }
  }
}
```

**What happens:** Veld starts `database:docker` first. Once the port accepts connections, `backend:local` starts with the Postgres connection string injected. Once the backend's `/health` returns 200, `frontend:local` starts with the backend's HTTPS URL wired into `NEXT_PUBLIC_API_URL`. Each service gets a URL like `https://frontend.feature-login.shopfront.localhost`.

---

## 2. Cross-Referencing URLs Without Cyclic Dependencies

**When to use:** The frontend needs the backend URL for API calls, and the backend needs the frontend URL for CORS origin configuration. Normally this would create a cycle -- each depends on the other. Veld solves this because `url` and `port` for all `start_server` nodes are pre-computed before any node executes.

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "portal",
  "url_template": "{service}.{branch ?? run}.portal.localhost",

  "nodes": {
    "backend": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @portal/api dev --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" },
          "env": {
            "CORS_ORIGIN": "${nodes.frontend.url}",
            "ALLOWED_ORIGINS": "${nodes.frontend.url},${nodes.admin.url}"
          }
        }
      }
    },

    "frontend": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @portal/web dev",
          "health_check": { "type": "http", "path": "/" },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_API_URL": "${nodes.backend.url}"
          }
        }
      }
    },

    "admin": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @portal/admin dev",
          "health_check": { "type": "http", "path": "/" },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_API_URL": "${nodes.backend.url}"
          }
        }
      }
    }
  }
}
```

**What happens:** Veld pre-computes URLs and ports for all three `start_server` nodes before starting anything. The backend receives `${nodes.frontend.url}` and `${nodes.admin.url}` as CORS origins even though neither frontend nor admin has started yet. No `depends_on` is needed between them -- all three start in parallel. This is only possible because the built-in `url` and `port` outputs are available before execution.

Note: This pre-computation only applies to the built-in `url` and `port` outputs. Custom outputs (from `$VELD_OUTPUT_FILE` or `VELD_OUTPUT` lines) still require the producing node to have executed first, so they still need `depends_on`.

---

## 3. Multiple Databases (Postgres + Redis)

**When to use:** Your backend needs both a relational database and a cache/queue. Both run as Docker containers.

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "taskboard",
  "url_template": "{service}.{branch ?? run}.taskboard.localhost",

  "nodes": {
    "postgres": {
      "variants": {
        "docker": {
          "type": "start_server",
          "command": "docker run --rm --name veld-pg-${veld.run} -e POSTGRES_PASSWORD=veld -e POSTGRES_DB=taskboard -p ${veld.port}:5432 postgres:16",
          "on_stop": "docker stop veld-pg-${veld.run}",
          "health_check": {
            "type": "command",
            "command": "docker exec veld-pg-${veld.run} pg_isready -U postgres",
            "timeout_seconds": 30,
            "interval_ms": 2000
          },
          "outputs": {
            "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/taskboard"
          }
        }
      }
    },

    "redis": {
      "variants": {
        "docker": {
          "type": "start_server",
          "command": "docker run --rm --name veld-redis-${veld.run} -p ${veld.port}:6379 redis:7-alpine",
          "on_stop": "docker stop veld-redis-${veld.run}",
          "health_check": {
            "type": "command",
            "command": "docker exec veld-redis-${veld.run} redis-cli ping",
            "timeout_seconds": 15
          },
          "outputs": {
            "REDIS_URL": "redis://localhost:${veld.port}/0"
          }
        }
      }
    },

    "api": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "cargo run --bin taskboard-api -- --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health", "timeout_seconds": 60 },
          "depends_on": {
            "postgres": "docker",
            "redis": "docker"
          },
          "env": {
            "DATABASE_URL": "${nodes.postgres.DATABASE_URL}",
            "REDIS_URL": "${nodes.redis.REDIS_URL}"
          }
        }
      }
    }
  }
}
```

**What happens:** Postgres and Redis start in parallel (they have no dependency on each other). The `api` node waits for both health checks to pass, then starts with both connection strings injected. The `command` health check type lets you use native readiness tools (`pg_isready`, `redis-cli ping`) instead of generic TCP port checks.

---

## 4. Local vs. Staging Backend with Variants

**When to use:** Frontend developers want to run their UI locally against either a local backend or a shared staging backend. Variants let them switch with a single flag.

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "dashboard",
  "url_template": "{service}.{branch ?? run}.dashboard.localhost",

  "presets": {
    "fullstack": ["frontend:local"],
    "ui-only": ["frontend:staging"]
  },

  "nodes": {
    "database": {
      "variants": {
        "docker": {
          "type": "start_server",
          "command": "docker run --rm --name veld-pg-${veld.run} -e POSTGRES_PASSWORD=veld -e POSTGRES_DB=dashboard -p ${veld.port}:5432 postgres:16",
          "on_stop": "docker stop veld-pg-${veld.run}",
          "health_check": { "type": "port", "timeout_seconds": 30 },
          "outputs": {
            "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/dashboard"
          }
        }
      }
    },

    "backend": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @dashboard/api dev --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" },
          "depends_on": { "database": "docker" },
          "env": {
            "DATABASE_URL": "${nodes.database.DATABASE_URL}"
          }
        },
        "staging": {
          "type": "command",
          "command": "echo 'BACKEND_URL=https://api.staging.dashboard.example.com' >> \"$VELD_OUTPUT_FILE\"",
          "outputs": ["BACKEND_URL"]
        }
      }
    },

    "frontend": {
      "default_variant": "local",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @dashboard/web dev",
          "health_check": { "type": "http", "path": "/" },
          "depends_on": { "backend": "local" },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_API_URL": "${nodes.backend:local.url}"
          }
        },
        "staging": {
          "type": "start_server",
          "command": "pnpm --filter @dashboard/web dev",
          "health_check": { "type": "http", "path": "/" },
          "depends_on": { "backend": "staging" },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_API_URL": "${nodes.backend:staging.BACKEND_URL}"
          }
        }
      }
    }
  }
}
```

**What happens:**

- `veld start --preset fullstack` resolves `frontend:local` -> `backend:local` -> `database:docker`. The full stack runs locally.
- `veld start --preset ui-only` resolves `frontend:staging` -> `backend:staging`. The staging variant is a `command` node that just emits the remote URL. No database starts. The frontend runs locally but talks to the staging API.

Note the qualified form `${nodes.backend:local.url}` and `${nodes.backend:staging.BACKEND_URL}`. Because two variants of `backend` exist in the config, Veld requires disambiguation even though only one runs at a time per preset.

---

## 5. Database Cloning with Idempotency

**When to use:** You clone a production or staging database into a local Postgres instance for development. The clone is expensive and should only run once unless the local copy is stale or missing.

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "analytics",
  "url_template": "{service}.{branch ?? run}.analytics.localhost",

  "nodes": {
    "postgres": {
      "variants": {
        "docker": {
          "type": "start_server",
          "command": "docker run --rm --name veld-pg-${veld.run} -e POSTGRES_PASSWORD=veld -p ${veld.port}:5432 -v veld-pg-data-${veld.run}:/var/lib/postgresql/data postgres:16",
          "on_stop": "docker stop veld-pg-${veld.run}",
          "health_check": {
            "type": "command",
            "command": "docker exec veld-pg-${veld.run} pg_isready -U postgres",
            "timeout_seconds": 30
          },
          "outputs": {
            "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/analytics"
          }
        }
      }
    },

    "clone-db": {
      "hidden": true,
      "variants": {
        "default": {
          "type": "command",
          "command": "pg_dump $SOURCE_DB_URL | psql ${nodes.postgres.DATABASE_URL}",
          "verify": "psql ${nodes.postgres.DATABASE_URL} -c 'SELECT 1 FROM users LIMIT 1'",
          "depends_on": { "postgres": "docker" },
          "env": {
            "SOURCE_DB_URL": "postgresql://readonly:secret@staging.analytics.example.com:5432/analytics"
          },
          "outputs": ["DATABASE_URL"],
          "sensitive_outputs": ["DATABASE_URL"]
        }
      }
    },

    "api": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @analytics/api dev --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" },
          "depends_on": { "clone-db": "default" },
          "env": {
            "DATABASE_URL": "${nodes.postgres.DATABASE_URL}"
          }
        }
      }
    }
  }
}
```

**What happens:** The `clone-db` node depends on `postgres:docker`. Before running the expensive `pg_dump | psql` pipeline, Veld executes the `verify` command. If the `users` table already has data (`SELECT 1` succeeds), the clone is skipped entirely. On the first run, the clone executes. On subsequent runs, it is a no-op. The `hidden: true` flag keeps `clone-db` out of `veld nodes` output since it is an internal concern.

---

## 6. Microservices with Complex Dependency Graph

**When to use:** Your system has multiple services with a non-trivial dependency graph. Some services depend on shared infrastructure; some depend on each other.

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "rideshare",
  "url_template": "{service}.{branch ?? run}.rideshare.localhost",

  "presets": {
    "full": ["gateway:local"],
    "riders-only": ["rider-service:local", "gateway:local"],
    "drivers-only": ["driver-service:local", "gateway:local"]
  },

  "nodes": {
    "postgres": {
      "variants": {
        "docker": {
          "type": "start_server",
          "command": "docker run --rm --name veld-pg-${veld.run} -e POSTGRES_PASSWORD=veld -p ${veld.port}:5432 postgres:16",
          "on_stop": "docker stop veld-pg-${veld.run}",
          "health_check": { "type": "port", "timeout_seconds": 30 },
          "outputs": {
            "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/rideshare"
          }
        }
      }
    },

    "redis": {
      "variants": {
        "docker": {
          "type": "start_server",
          "command": "docker run --rm --name veld-redis-${veld.run} -p ${veld.port}:6379 redis:7-alpine",
          "on_stop": "docker stop veld-redis-${veld.run}",
          "health_check": { "type": "port", "timeout_seconds": 15 },
          "outputs": {
            "REDIS_URL": "redis://localhost:${veld.port}/0"
          }
        }
      }
    },

    "rider-service": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "cargo run --bin rider-service -- --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health", "timeout_seconds": 60 },
          "depends_on": { "postgres": "docker", "redis": "docker" },
          "env": {
            "DATABASE_URL": "${nodes.postgres.DATABASE_URL}",
            "REDIS_URL": "${nodes.redis.REDIS_URL}",
            "PRICING_SERVICE_URL": "${nodes.pricing-service.url}"
          }
        }
      }
    },

    "driver-service": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "cargo run --bin driver-service -- --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health", "timeout_seconds": 60 },
          "depends_on": { "postgres": "docker", "redis": "docker" },
          "env": {
            "DATABASE_URL": "${nodes.postgres.DATABASE_URL}",
            "REDIS_URL": "${nodes.redis.REDIS_URL}"
          }
        }
      }
    },

    "pricing-service": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "cargo run --bin pricing-service -- --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health", "timeout_seconds": 60 },
          "depends_on": { "redis": "docker" },
          "env": {
            "REDIS_URL": "${nodes.redis.REDIS_URL}"
          }
        }
      }
    },

    "notification-service": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "cargo run --bin notification-service -- --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health", "timeout_seconds": 60 },
          "depends_on": { "redis": "docker" },
          "env": {
            "REDIS_URL": "${nodes.redis.REDIS_URL}"
          }
        }
      }
    },

    "gateway": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "cargo run --bin gateway -- --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" },
          "depends_on": {
            "rider-service": "local",
            "driver-service": "local",
            "pricing-service": "local",
            "notification-service": "local"
          },
          "env": {
            "RIDER_SERVICE_URL": "${nodes.rider-service.url}",
            "DRIVER_SERVICE_URL": "${nodes.driver-service.url}",
            "PRICING_SERVICE_URL": "${nodes.pricing-service.url}",
            "NOTIFICATION_SERVICE_URL": "${nodes.notification-service.url}"
          }
        }
      }
    }
  }
}
```

**What happens:** The dependency graph is resolved as a DAG:

1. `postgres` and `redis` start in parallel (no dependencies).
2. `pricing-service` and `notification-service` start in parallel once `redis` is healthy.
3. `rider-service` and `driver-service` start in parallel once both `postgres` and `redis` are healthy.
4. `gateway` starts last, after all four services are healthy.

Notice that `rider-service` references `${nodes.pricing-service.url}` in its env without declaring a `depends_on` for it. This works because `url` is a pre-computed built-in output -- it is available before `pricing-service` starts. The `depends_on` on `postgres` and `redis` is still needed because those provide custom outputs (`DATABASE_URL`, `REDIS_URL`) that require execution.

The presets let you run subsets: `--preset riders-only` starts only the rider service and gateway (plus their infrastructure dependencies).

---

## 7. Monorepo with Shared Setup Steps

**When to use:** Multiple services share setup steps -- certificate generation, database migrations, seed data. These run once before any server starts.

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "enterprise-app",
  "url_template": "{service}.{branch ?? run}.enterprise-app.localhost",

  "nodes": {
    "postgres": {
      "variants": {
        "docker": {
          "type": "start_server",
          "command": "docker run --rm --name veld-pg-${veld.run} -e POSTGRES_PASSWORD=veld -e POSTGRES_DB=enterprise -p ${veld.port}:5432 postgres:16",
          "on_stop": "docker stop veld-pg-${veld.run}",
          "health_check": {
            "type": "command",
            "command": "docker exec veld-pg-${veld.run} pg_isready -U postgres",
            "timeout_seconds": 30
          },
          "outputs": {
            "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/enterprise"
          }
        }
      }
    },

    "generate-certs": {
      "hidden": true,
      "variants": {
        "default": {
          "type": "command",
          "command": "./scripts/generate-dev-certs.sh",
          "verify": "test -f ./certs/dev.pem && test -f ./certs/dev-key.pem",
          "outputs": ["CERT_PATH", "KEY_PATH"]
        }
      }
    },

    "migrate-db": {
      "hidden": true,
      "variants": {
        "default": {
          "type": "command",
          "command": "pnpm --filter @enterprise/db migrate:dev",
          "depends_on": { "postgres": "docker" },
          "env": {
            "DATABASE_URL": "${nodes.postgres.DATABASE_URL}"
          },
          "verify": "pnpm --filter @enterprise/db migrate:status --exit-code"
        }
      }
    },

    "seed-db": {
      "hidden": true,
      "variants": {
        "default": {
          "type": "command",
          "command": "pnpm --filter @enterprise/db seed",
          "depends_on": { "migrate-db": "default" },
          "env": {
            "DATABASE_URL": "${nodes.postgres.DATABASE_URL}"
          },
          "verify": "pnpm --filter @enterprise/db seed:check"
        }
      }
    },

    "backend": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @enterprise/api dev --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" },
          "depends_on": {
            "seed-db": "default",
            "generate-certs": "default"
          },
          "env": {
            "DATABASE_URL": "${nodes.postgres.DATABASE_URL}",
            "TLS_CERT": "${nodes.generate-certs.CERT_PATH}",
            "TLS_KEY": "${nodes.generate-certs.KEY_PATH}"
          }
        }
      }
    },

    "frontend": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @enterprise/web dev",
          "health_check": { "type": "http", "path": "/" },
          "depends_on": { "backend": "local" },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_API_URL": "${nodes.backend.url}"
          }
        }
      }
    }
  }
}
```

**What happens:**

1. `postgres:docker` and `generate-certs:default` start in parallel (independent).
2. Once Postgres is ready, `migrate-db` runs (skipped if migrations are current, thanks to `verify`).
3. Once migrations complete, `seed-db` runs (skipped if seed data exists).
4. Once both `seed-db` and `generate-certs` finish, `backend:local` starts.
5. Finally, `frontend:local` starts.

The `verify` commands on the setup nodes make subsequent `veld start` calls fast -- if certs exist, migrations are current, and seed data is present, all three setup steps are skipped in milliseconds.

---

## 8. Docker Infrastructure with Synthetic Outputs

**When to use:** You run Postgres, Redis, and Elasticsearch as Docker containers. Each needs to expose a connection string to downstream nodes. Since Docker containers cannot write to `$VELD_OUTPUT_FILE` in a way Veld captures, you use synthetic outputs.

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "search-platform",
  "url_template": "{service}.{branch ?? run}.search-platform.localhost",

  "nodes": {
    "postgres": {
      "variants": {
        "docker": {
          "type": "start_server",
          "command": "docker run --rm --name veld-pg-${veld.run} -e POSTGRES_PASSWORD=veld -e POSTGRES_DB=search_platform -p ${veld.port}:5432 postgres:16",
          "on_stop": "docker stop veld-pg-${veld.run}",
          "health_check": { "type": "port", "timeout_seconds": 30 },
          "outputs": {
            "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/search_platform",
            "JDBC_URL": "jdbc:postgresql://localhost:${veld.port}/search_platform"
          }
        }
      }
    },

    "redis": {
      "variants": {
        "docker": {
          "type": "start_server",
          "command": "docker run --rm --name veld-redis-${veld.run} -p ${veld.port}:6379 redis:7-alpine --appendonly yes",
          "on_stop": "docker stop veld-redis-${veld.run}",
          "health_check": { "type": "port", "timeout_seconds": 15 },
          "outputs": {
            "REDIS_URL": "redis://localhost:${veld.port}/0",
            "REDIS_CACHE_URL": "redis://localhost:${veld.port}/1",
            "REDIS_QUEUE_URL": "redis://localhost:${veld.port}/2"
          }
        }
      }
    },

    "elasticsearch": {
      "variants": {
        "docker": {
          "type": "start_server",
          "command": "docker run --rm --name veld-es-${veld.run} -e discovery.type=single-node -e xpack.security.enabled=false -e ES_JAVA_OPTS='-Xms512m -Xmx512m' -p ${veld.port}:9200 elasticsearch:8.13.0",
          "on_stop": "docker stop veld-es-${veld.run}",
          "health_check": {
            "type": "http",
            "path": "/_cluster/health",
            "timeout_seconds": 90,
            "interval_ms": 3000
          },
          "outputs": {
            "ELASTICSEARCH_URL": "http://localhost:${veld.port}"
          }
        }
      }
    },

    "api": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @search-platform/api dev --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" },
          "depends_on": {
            "postgres": "docker",
            "redis": "docker",
            "elasticsearch": "docker"
          },
          "env": {
            "DATABASE_URL": "${nodes.postgres.DATABASE_URL}",
            "REDIS_URL": "${nodes.redis.REDIS_URL}",
            "REDIS_CACHE_URL": "${nodes.redis.REDIS_CACHE_URL}",
            "REDIS_QUEUE_URL": "${nodes.redis.REDIS_QUEUE_URL}",
            "ELASTICSEARCH_URL": "${nodes.elasticsearch.ELASTICSEARCH_URL}"
          }
        }
      }
    }
  }
}
```

**What happens:** All three infrastructure containers start in parallel. Synthetic outputs are template strings that are interpolated after port allocation -- no `$VELD_OUTPUT_FILE` writes needed. The `api` node depends on all three and receives five connection strings. Note the Redis node exposes three different database numbers for different concerns (main, cache, queue).

---

## 9. Feature Branch Isolation

**When to use:** You want every feature branch to get its own unique URLs so multiple developers (or multiple branches on one machine) never collide.

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "crm",
  "url_template": "{service}.{branch ?? run}.crm.localhost",

  "nodes": {
    "database": {
      "variants": {
        "docker": {
          "type": "start_server",
          "command": "docker run --rm --name veld-pg-${veld.branch}-${veld.run} -e POSTGRES_PASSWORD=veld -e POSTGRES_DB=crm_${veld.branch} -p ${veld.port}:5432 -v veld-pg-${veld.branch}:/var/lib/postgresql/data postgres:16",
          "on_stop": "docker stop veld-pg-${veld.branch}-${veld.run}",
          "health_check": { "type": "port", "timeout_seconds": 30 },
          "outputs": {
            "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/crm_${veld.branch}"
          }
        }
      }
    },

    "backend": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @crm/api dev --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" },
          "depends_on": { "database": "docker" },
          "env": {
            "DATABASE_URL": "${nodes.database.DATABASE_URL}"
          }
        }
      }
    },

    "frontend": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @crm/web dev",
          "health_check": { "type": "http", "path": "/" },
          "depends_on": { "backend": "local" },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_API_URL": "${nodes.backend.url}"
          }
        }
      }
    }
  }
}
```

**What happens:** The `{branch ?? run}` in the URL template means:

- On branch `feature/user-profiles`, the frontend URL becomes `https://frontend.feature-user-profiles.crm.localhost`.
- On branch `fix/auth-bug`, it becomes `https://frontend.fix-auth-bug.crm.localhost`.
- If not in a git repo, it falls back to the run name.

Each branch also gets its own Docker volume (`veld-pg-${veld.branch}`) and database name, so branch data never collides. When you switch branches and run `veld start`, you get a completely isolated environment.

---

## 10. Multi-Developer / Team URL Isolation

**When to use:** Multiple developers share a machine or a namespace (e.g., a shared Caddy proxy or a team `.localhost` domain) and need non-colliding URLs.

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "inventory",
  "url_template": "{service}.{username}.{branch ?? run}.inventory.localhost",

  "nodes": {
    "backend": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "go run ./cmd/server --port ${veld.port}",
          "health_check": { "type": "http", "path": "/healthz" },
          "env": {
            "FRONTEND_URL": "${nodes.frontend.url}"
          }
        }
      }
    },

    "frontend": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @inventory/web dev",
          "health_check": { "type": "http", "path": "/" },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_API_URL": "${nodes.backend.url}"
          }
        }
      }
    }
  }
}
```

**What happens:** The `{username}` placeholder in the URL template means developer `alice` on branch `main` gets `https://frontend.alice.main.inventory.localhost`, while developer `bob` on the same branch gets `https://frontend.bob.main.inventory.localhost`. Ports are also independently allocated per run, so there is zero collision even when running simultaneously on the same host.

---

## 11. Sensitive Credentials

**When to use:** A setup step produces credentials (database passwords, API keys, tokens) that should not appear in logs or be stored in plaintext.

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "fintech",
  "url_template": "{service}.{branch ?? run}.fintech.localhost",

  "nodes": {
    "provision-db": {
      "hidden": true,
      "variants": {
        "default": {
          "type": "command",
          "script": "./scripts/provision-dev-db.sh",
          "verify": "./scripts/check-dev-db.sh",
          "on_stop": "./scripts/teardown-dev-db.sh",
          "outputs": ["DATABASE_URL", "DB_PASSWORD", "DB_READONLY_URL"],
          "sensitive_outputs": ["DATABASE_URL", "DB_PASSWORD", "DB_READONLY_URL"]
        }
      }
    },

    "fetch-api-keys": {
      "hidden": true,
      "variants": {
        "default": {
          "type": "command",
          "command": "vault read -format=json secret/dev/payment-gateway | jq -r '.data | to_entries[] | \"\\(.key)=\\(.value)\"' >> \"$VELD_OUTPUT_FILE\"",
          "outputs": ["STRIPE_SECRET_KEY", "STRIPE_WEBHOOK_SECRET"],
          "sensitive_outputs": ["STRIPE_SECRET_KEY", "STRIPE_WEBHOOK_SECRET"]
        }
      }
    },

    "api": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @fintech/api dev --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" },
          "depends_on": {
            "provision-db": "default",
            "fetch-api-keys": "default"
          },
          "env": {
            "DATABASE_URL": "${nodes.provision-db.DATABASE_URL}",
            "STRIPE_SECRET_KEY": "${nodes.fetch-api-keys.STRIPE_SECRET_KEY}",
            "STRIPE_WEBHOOK_SECRET": "${nodes.fetch-api-keys.STRIPE_WEBHOOK_SECRET}"
          }
        }
      }
    }
  }
}
```

**What happens:** The `provision-db` script creates a temporary database and emits connection strings. The `fetch-api-keys` step reads secrets from Vault. Both declare `sensitive_outputs`, which means:

- All six output values are masked as `[REDACTED]` in terminal output, `veld logs`, and debug logs.
- Values are encrypted at rest using a machine-local key.
- The `veld graph` command never shows these values.

The `api` node receives the secrets as environment variables at runtime.

---

## 12. Custom Apex Domains

**When to use:** You want services at a real-looking domain (e.g., `*.myapp.dev`) instead of `.localhost`. This is useful for cookie scoping, CORS testing, or matching production domain structures.

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "saas-platform",
  "url_template": "{service}.{branch ?? run}.saas-platform.test",

  "nodes": {
    "api": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "go run ./cmd/api --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" },
          "env": {
            "COOKIE_DOMAIN": ".saas-platform.test",
            "CORS_ORIGINS": "${nodes.web.url},${nodes.admin.url}"
          }
        }
      }
    },

    "web": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @saas/web dev",
          "health_check": { "type": "http", "path": "/" },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_API_URL": "${nodes.api.url}"
          }
        }
      }
    },

    "admin": {
      "url_template": "admin.{branch ?? run}.saas-platform.test",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @saas/admin dev",
          "health_check": { "type": "http", "path": "/" },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_API_URL": "${nodes.api.url}"
          }
        }
      }
    }
  }
}
```

**What happens:** Veld generates URLs like `https://api.feature-auth.saas-platform.test` and `https://admin.feature-auth.saas-platform.test`. Because `.test` is not `.localhost`, Veld manages DNS entries via `veld-helper` (writing exact host entries, never wildcards).

**Requirement:** Custom apex domains require `veld setup privileged`. If you are in unprivileged mode, `veld start` will exit with an error explaining that non-`.localhost` domains need privileged setup for `/etc/hosts` access. The error message tells you exactly what to run.

The `admin` node uses a node-level `url_template` override to produce a different URL shape than the project default — for example, `admin.saas-platform.test` instead of the default `{service}.{branch ?? run}.saas-platform.test` pattern.

---

## 13. Hybrid Local/Remote Services

**When to use:** You are working on one or two services locally but want the rest of the stack to point at staging or production. Common when a system has 10+ microservices and you only need to modify one.

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "marketplace",
  "url_template": "{service}.{branch ?? run}.marketplace.localhost",

  "presets": {
    "catalog-dev": ["catalog-service:local", "gateway:local"],
    "payments-dev": ["payment-service:local", "gateway:local"],
    "full-local": ["gateway:local-full"]
  },

  "nodes": {
    "user-service": {
      "default_variant": "staging",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "cargo run --bin user-service -- --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health", "timeout_seconds": 60 }
        },
        "staging": {
          "type": "command",
          "command": "echo 'SERVICE_URL=https://user-service.staging.marketplace.example.com' >> \"$VELD_OUTPUT_FILE\"",
          "outputs": ["SERVICE_URL"]
        }
      }
    },

    "catalog-service": {
      "default_variant": "staging",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "cargo run --bin catalog-service -- --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health", "timeout_seconds": 60 }
        },
        "staging": {
          "type": "command",
          "command": "echo 'SERVICE_URL=https://catalog-service.staging.marketplace.example.com' >> \"$VELD_OUTPUT_FILE\"",
          "outputs": ["SERVICE_URL"]
        }
      }
    },

    "payment-service": {
      "default_variant": "staging",
      "variants": {
        "local": {
          "type": "start_server",
          "command": "cargo run --bin payment-service -- --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health", "timeout_seconds": 60 }
        },
        "staging": {
          "type": "command",
          "command": "echo 'SERVICE_URL=https://payment-service.staging.marketplace.example.com' >> \"$VELD_OUTPUT_FILE\"",
          "outputs": ["SERVICE_URL"]
        }
      }
    },

    "notification-service": {
      "default_variant": "staging",
      "variants": {
        "staging": {
          "type": "command",
          "command": "echo 'SERVICE_URL=https://notification-service.staging.marketplace.example.com' >> \"$VELD_OUTPUT_FILE\"",
          "outputs": ["SERVICE_URL"]
        }
      }
    },

    "gateway": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "cargo run --bin gateway -- --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" },
          "depends_on": {
            "user-service": "staging",
            "catalog-service": "local",
            "payment-service": "staging",
            "notification-service": "staging"
          },
          "env": {
            "USER_SERVICE_URL": "${nodes.user-service:staging.SERVICE_URL}",
            "CATALOG_SERVICE_URL": "${nodes.catalog-service:local.url}",
            "PAYMENT_SERVICE_URL": "${nodes.payment-service:staging.SERVICE_URL}",
            "NOTIFICATION_SERVICE_URL": "${nodes.notification-service:staging.SERVICE_URL}"
          }
        },
        "local-full": {
          "type": "start_server",
          "command": "cargo run --bin gateway -- --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" },
          "depends_on": {
            "user-service": "local",
            "catalog-service": "local",
            "payment-service": "local",
            "notification-service": "staging"
          },
          "env": {
            "USER_SERVICE_URL": "${nodes.user-service:local.url}",
            "CATALOG_SERVICE_URL": "${nodes.catalog-service:local.url}",
            "PAYMENT_SERVICE_URL": "${nodes.payment-service:local.url}",
            "NOTIFICATION_SERVICE_URL": "${nodes.notification-service:staging.SERVICE_URL}"
          }
        }
      }
    }
  }
}
```

**What happens:** With `--preset catalog-dev`, the gateway runs locally and routes most traffic to staging services. Only `catalog-service` runs locally. The staging variants are lightweight `command` nodes that instantly emit the remote URL -- no servers start for those services. This gives you a fast feedback loop on the service you are changing while keeping the rest of the system real.

Note the use of qualified references (`${nodes.catalog-service:local.url}` vs `${nodes.user-service:staging.SERVICE_URL}`) since multiple variants of the same node may be referenced across different gateway variants.

---

## 14. Build Step Before Server

**When to use:** A service needs a compile or build step before it can be served. The build output is a static artifact (binary, bundle, etc.) that the server then uses.

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "docs-site",
  "url_template": "{service}.{branch ?? run}.docs-site.localhost",

  "nodes": {
    "build-docs": {
      "hidden": true,
      "variants": {
        "default": {
          "type": "command",
          "command": "pnpm --filter @docs-site/content build",
          "verify": "test -d ./packages/content/dist && test ./packages/content/dist/index.html -nt ./packages/content/src/index.md"
        }
      }
    },

    "build-api": {
      "hidden": true,
      "variants": {
        "default": {
          "type": "command",
          "command": "cargo build --release --bin docs-api",
          "verify": "test -f ./target/release/docs-api && test ./target/release/docs-api -nt ./src/main.rs"
        }
      }
    },

    "api": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "./target/release/docs-api --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" },
          "depends_on": { "build-api": "default" }
        }
      }
    },

    "docs": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "python3 -m http.server ${veld.port} --directory ./packages/content/dist",
          "health_check": { "type": "http", "path": "/" },
          "depends_on": { "build-docs": "default" }
        }
      }
    }
  }
}
```

**What happens:** The `build-docs` and `build-api` command nodes run first (in parallel, since they are independent). The `verify` commands check whether the build artifacts are newer than the source files -- if so, the builds are skipped. Then `docs:local` serves the static files and `api:local` runs the compiled binary. On first run, both builds execute. On subsequent runs, they are skipped unless source files changed.

---

## 15. Worker Processes Alongside API Servers

**When to use:** Your application has background workers (job processors, event consumers, schedulers) that run alongside the API server and share the same infrastructure.

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "jobrunner",
  "url_template": "{service}.{branch ?? run}.jobrunner.localhost",

  "presets": {
    "full": ["api:local", "worker:local", "scheduler:local"],
    "api-only": ["api:local"]
  },

  "nodes": {
    "postgres": {
      "variants": {
        "docker": {
          "type": "start_server",
          "command": "docker run --rm --name veld-pg-${veld.run} -e POSTGRES_PASSWORD=veld -e POSTGRES_DB=jobrunner -p ${veld.port}:5432 postgres:16",
          "on_stop": "docker stop veld-pg-${veld.run}",
          "health_check": { "type": "port", "timeout_seconds": 30 },
          "outputs": {
            "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/jobrunner"
          }
        }
      }
    },

    "redis": {
      "variants": {
        "docker": {
          "type": "start_server",
          "command": "docker run --rm --name veld-redis-${veld.run} -p ${veld.port}:6379 redis:7-alpine",
          "on_stop": "docker stop veld-redis-${veld.run}",
          "health_check": { "type": "port", "timeout_seconds": 15 },
          "outputs": {
            "REDIS_URL": "redis://localhost:${veld.port}/0"
          }
        }
      }
    },

    "api": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @jobrunner/api dev --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" },
          "depends_on": {
            "postgres": "docker",
            "redis": "docker"
          },
          "env": {
            "DATABASE_URL": "${nodes.postgres.DATABASE_URL}",
            "REDIS_URL": "${nodes.redis.REDIS_URL}",
            "WORKER_DASHBOARD_URL": "${nodes.worker.url}"
          }
        }
      }
    },

    "worker": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @jobrunner/worker start --port ${veld.port} --concurrency 5",
          "health_check": { "type": "http", "path": "/status" },
          "depends_on": {
            "postgres": "docker",
            "redis": "docker"
          },
          "env": {
            "DATABASE_URL": "${nodes.postgres.DATABASE_URL}",
            "REDIS_URL": "${nodes.redis.REDIS_URL}",
            "API_URL": "${nodes.api.url}"
          }
        }
      }
    },

    "scheduler": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @jobrunner/scheduler start --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" },
          "depends_on": {
            "redis": "docker"
          },
          "env": {
            "REDIS_URL": "${nodes.redis.REDIS_URL}",
            "API_URL": "${nodes.api.url}"
          }
        }
      }
    }
  }
}
```

**What happens:** With `--preset full`:

1. Postgres and Redis start in parallel.
2. Once both are healthy, `api`, `worker`, and `scheduler` can start. The `api` and `worker` nodes both depend on Postgres and Redis, so they start as soon as infrastructure is ready. The `scheduler` depends only on Redis.
3. Note the cross-references: `api` references `${nodes.worker.url}` (for a worker dashboard link) and `worker` references `${nodes.api.url}` (for callback URLs). Neither has a `depends_on` on the other -- this works because `url` is pre-computed.

With `--preset api-only`, only the API and its infrastructure dependencies start. No worker, no scheduler.

All three application nodes are `start_server` with health check endpoints, so Veld monitors them and reports if any crashes.

---

## 16. Polyglot Monorepo (Mixed Languages)

**When to use:** Your project has services written in different languages with different build tools and runtimes.

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "polyglot",
  "url_template": "{service}.{branch ?? run}.polyglot.localhost",

  "nodes": {
    "postgres": {
      "variants": {
        "docker": {
          "type": "start_server",
          "command": "docker run --rm --name veld-pg-${veld.run} -e POSTGRES_PASSWORD=veld -e POSTGRES_DB=polyglot -p ${veld.port}:5432 postgres:16",
          "on_stop": "docker stop veld-pg-${veld.run}",
          "health_check": { "type": "port", "timeout_seconds": 30 },
          "outputs": {
            "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/polyglot",
            "JDBC_URL": "jdbc:postgresql://localhost:${veld.port}/polyglot"
          }
        }
      }
    },

    "auth-service": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "cd services/auth && go run . --port ${veld.port}",
          "health_check": { "type": "http", "path": "/healthz" },
          "depends_on": { "postgres": "docker" },
          "env": {
            "DATABASE_URL": "${nodes.postgres.DATABASE_URL}"
          }
        }
      }
    },

    "billing-service": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "cd services/billing && ./gradlew bootRun --args='--server.port=${veld.port}'",
          "health_check": {
            "type": "http",
            "path": "/actuator/health",
            "timeout_seconds": 90,
            "interval_ms": 3000
          },
          "depends_on": { "postgres": "docker" },
          "env": {
            "SPRING_DATASOURCE_URL": "${nodes.postgres.JDBC_URL}",
            "SPRING_DATASOURCE_USERNAME": "postgres",
            "SPRING_DATASOURCE_PASSWORD": "veld"
          }
        }
      }
    },

    "recommendation-service": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "cd services/recommendations && uvicorn main:app --host 0.0.0.0 --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" },
          "depends_on": { "postgres": "docker" },
          "env": {
            "DATABASE_URL": "${nodes.postgres.DATABASE_URL}"
          }
        }
      }
    },

    "frontend": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @polyglot/web dev",
          "health_check": { "type": "http", "path": "/" },
          "depends_on": {
            "auth-service": "local",
            "billing-service": "local",
            "recommendation-service": "local"
          },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_AUTH_URL": "${nodes.auth-service.url}",
            "NEXT_PUBLIC_BILLING_URL": "${nodes.billing-service.url}",
            "NEXT_PUBLIC_RECOMMENDATIONS_URL": "${nodes.recommendation-service.url}"
          }
        }
      }
    }
  }
}
```

**What happens:** Veld does not care what language your services are written in. Go, Java/Kotlin (Spring Boot), Python (FastAPI/uvicorn), and Node.js all work the same way -- they receive `${veld.port}`, bind to it, and Veld health-checks them. Note the longer `timeout_seconds` and `interval_ms` for the Spring Boot service, since JVM startup is slower. Each service gets the same HTTPS URL treatment regardless of its runtime.

---

## 17. End-to-End Test Runner

**When to use:** You want to run the full stack and then execute end-to-end tests against it. The test runner is a `command` node that depends on the entire stack being healthy.

```json
{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "e2e-suite",
  "url_template": "{service}.{branch ?? run}.e2e-suite.localhost",

  "presets": {
    "dev": ["frontend:local"],
    "test": ["e2e:default"]
  },

  "nodes": {
    "postgres": {
      "variants": {
        "docker": {
          "type": "start_server",
          "command": "docker run --rm --name veld-pg-${veld.run} -e POSTGRES_PASSWORD=veld -e POSTGRES_DB=e2e -p ${veld.port}:5432 postgres:16",
          "on_stop": "docker stop veld-pg-${veld.run}",
          "health_check": { "type": "port", "timeout_seconds": 30 },
          "outputs": {
            "DATABASE_URL": "postgresql://postgres:veld@localhost:${veld.port}/e2e"
          }
        }
      }
    },

    "seed-test-data": {
      "hidden": true,
      "variants": {
        "default": {
          "type": "command",
          "command": "pnpm --filter @e2e-suite/db seed:test",
          "depends_on": { "postgres": "docker" },
          "env": {
            "DATABASE_URL": "${nodes.postgres.DATABASE_URL}"
          }
        }
      }
    },

    "backend": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @e2e-suite/api dev --port ${veld.port}",
          "health_check": { "type": "http", "path": "/health" },
          "depends_on": { "seed-test-data": "default" },
          "env": {
            "DATABASE_URL": "${nodes.postgres.DATABASE_URL}",
            "NODE_ENV": "test"
          }
        }
      }
    },

    "frontend": {
      "variants": {
        "local": {
          "type": "start_server",
          "command": "pnpm --filter @e2e-suite/web dev",
          "health_check": { "type": "http", "path": "/" },
          "depends_on": { "backend": "local" },
          "env": {
            "PORT": "${veld.port}",
            "NEXT_PUBLIC_API_URL": "${nodes.backend.url}"
          }
        }
      }
    },

    "e2e": {
      "variants": {
        "default": {
          "type": "command",
          "command": "pnpm --filter @e2e-suite/tests playwright test",
          "depends_on": { "frontend": "local" },
          "env": {
            "BASE_URL": "${nodes.frontend.url}",
            "API_URL": "${nodes.backend.url}"
          },
          "outputs": ["TEST_RESULTS_PATH"],
          "strict_outputs": false
        }
      }
    }
  }
}
```

**What happens:** With `--preset test`:

1. Postgres starts.
2. Test seed data is inserted.
3. Backend starts with `NODE_ENV=test`.
4. Frontend starts.
5. Once the frontend is healthy, the `e2e` command node runs Playwright tests against the live stack.

The `e2e` node is a `command` type, so it runs to completion and Veld reports its exit code. The `strict_outputs: false` flag means it will not fail if the test runner does not emit a `TEST_RESULTS_PATH` output.

With `--preset dev`, only the frontend and its dependencies start (no test runner), giving you the same stack for manual development.
