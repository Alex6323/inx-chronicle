---
keywords:
- documentation
- api
- reference
- jwt
- authentication
---

# Authentication

Usage of the Chronicle API can be protected using [JWT](https://jwt.io/), by setting the following configuration settings under the `[api]` table in [config.toml](https://github.com/iotaledger/inx-chronicle/blob/main/config.template.toml).

- `password_hash` - The [argon2](https://argon2.online/) hash of your chosen password.
- `password_salt` - The salt used to hash the above password.
- `identity_path` - The path to an [EdDSA](https://en.wikipedia.org/wiki/EdDSA) secret key.
- `jwt_expiration` - The duration for which the JWT token is valid.
- `public_routes` - A list of routes that can be accessed without providing a token. These can include the wildcard (*) symbol to allow any sequence of characters to match.

The `[api.argon_config]` sub-table can be used to configure the argon2 hash generation, using the following settings:

- `hash_length` - The length of the resulting hash in bytes.
- `parallelism` - The number of threads used.
- `mem_cost` - The memory used.
- `iterations` - The number of iterations to perform.
- `variant` - One of three variants: [argon2d](https://www.argon2d.com/), [argon2i](https://www.argon2i.com/), or [argon2id](https://www.argon2id.com/).
- `version` - The argon version (currently 0x13).

All JWT interactions should be performed via HTTPS.

## Public Routes

When a route is configured to be public, it can be accessed freely without providing a JWT. Thus, you should take care when specifying these routes, as a mis-configured route can open the application up to attacks. The only accepted special character is the wildcard (`*`), which will be converted to a regex `.*` and match against the original URI.

For instance, a request `GET https://localhost:XXXX/api/core/v2/milestones/by-index/10000` will check the set of public routes against the segment `/api/core/v2/milestones/by-index/10000`. 

Matching strings include:

- `/api/*`
- `/api/core/*/milestones/by-index/*`
- `*10000`

Non-matching strings include:

- `/core/v2/milestones/by-index/*`
- `/api/core/v2/milestones/by-index`
- `/api/core/v1/*`

If JWT is used, these routes should be as specific as possible to avoid accidentally exposing unintended routes.

## Keys

Chronicle uses an EdDSA secret key to create tokens, which can be generated by the application at startup or provided as an identity file using the `identity_path` config. Currently, this file must be a PKCS8 secret key ([RFC 5208](https://datatracker.ietf.org/doc/html/rfc5208)) PEM file. The location of this file can also optionally be specified using the `IDENTITY_PATH` env variable, which will be overridden by the config file value. If no such file is provided, a secret key is randomly generated for use while the application is running.

## Generating a Token

A special route at the root (`/login`) is provided for generating a new token. This token will use the password config as well as the `jwt_expiration` and the secret key. This token can be manually generated by the client, if desired, by using the same identity and claims.

Static claims used by Chronicle are:

- `iss`: `"chronicle"`
- `aud`: `"api"`

The `sub` (subject) claim is filled using a unique UUID, however it is not currently stored or validated by Chronicle.

## Providing a Token

To provide a token when making a request, include it in an `Authorization` header using the `Bearer` authentication scheme.
