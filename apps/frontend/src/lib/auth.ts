import NextAuth from "next-auth";
import Discord from "next-auth/providers/discord";
import type { DiscordProfile } from "@auth/core/providers/discord";
import { cookies } from "next/headers";
import { Pool } from "pg";
import type {
  Adapter,
  AdapterUser,
  AdapterAccount,
  AdapterSession,
} from "@auth/core/adapters";

const pool = new Pool({ connectionString: process.env.DATABASE_URL });

function extractDiscordProfile(profile: DiscordProfile) {
  const avatarUrl = profile.avatar
    ? `https://cdn.discordapp.com/avatars/${profile.id}/${profile.avatar}.webp`
    : null;
  const name = profile.global_name ?? profile.username ?? null;
  return { avatarUrl, name };
}

function toAdapterUser(row: Record<string, unknown>): AdapterUser {
  return {
    id: row.id as string,
    name: (row.display_name as string) ?? null,
    email: (row.primary_email as string) ?? "",
    emailVerified: null,
    image: (row.provider_image as string) ?? null,
  };
}

function pgAdapter(): Adapter {
  return {
    async createUser(user) {
      const cookieStore = await cookies();
      const tz = cookieStore.get("browser_tz")?.value ?? null;

      let timezone = "UTC";
      if (tz) {
        try {
          Intl.DateTimeFormat("en-US", { timeZone: tz });
          timezone = tz;
        } catch {
          // invalid timezone cookie → UTC fallback
        }
      }

      const result = await pool.query(
        `INSERT INTO users (display_name, primary_email, timezone)
         VALUES ($1, $2, $3)
         RETURNING id, display_name, primary_email`,
        [user.name ?? "User", user.email ?? null, timezone]
      );
      return toAdapterUser(result.rows[0]);
    },

    async getUser(id) {
      const result = await pool.query(
        `SELECT u.id, u.display_name, u.primary_email, a.provider_image
         FROM users u
         LEFT JOIN accounts a ON a.user_id = u.id AND a.provider = 'discord'
         WHERE u.id = $1`,
        [id]
      );
      return result.rows[0] ? toAdapterUser(result.rows[0]) : null;
    },

    async getUserByEmail(_email) {
      // Email-based linking is not used for OAuth. Always return null.
      return null;
    },

    async getUserByAccount({ provider, providerAccountId }) {
      const result = await pool.query(
        `SELECT u.id, u.display_name, u.primary_email, a.provider_image
         FROM users u
         JOIN accounts a ON a.user_id = u.id
         WHERE a.provider = $1 AND a.provider_account_id = $2`,
        [provider, providerAccountId]
      );
      return result.rows[0] ? toAdapterUser(result.rows[0]) : null;
    },

    async updateUser(user) {
      const result = await pool.query(
        `UPDATE users SET
          display_name = COALESCE($1, display_name),
          primary_email = COALESCE($2, primary_email),
          updated_at = now()
         WHERE id = $3
         RETURNING id, display_name, primary_email`,
        [user.name, user.email, user.id]
      );
      return toAdapterUser(result.rows[0]);
    },

    async linkAccount(account: AdapterAccount) {
      await pool.query(
        `INSERT INTO accounts (
          user_id, provider, provider_account_id, account_type,
          access_token, refresh_token, expires_at, token_type, scope, id_token
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)`,
        [
          account.userId,
          account.provider,
          account.providerAccountId,
          account.type,
          account.access_token ?? null,
          account.refresh_token ?? null,
          account.expires_at ?? null,
          account.token_type ?? null,
          account.scope ?? null,
          account.id_token ?? null,
        ]
      );
    },

    async createSession(session) {
      await pool.query(
        `INSERT INTO sessions (session_token, user_id, expires)
         VALUES ($1, $2, $3)`,
        [session.sessionToken, session.userId, session.expires]
      );
      return session as AdapterSession;
    },

    async getSessionAndUser(sessionToken) {
      const result = await pool.query(
        `SELECT
          s.session_token, s.user_id, s.expires,
          u.id, u.display_name, u.primary_email,
          a.provider_image
         FROM sessions s
         JOIN users u ON s.user_id = u.id
         LEFT JOIN accounts a ON a.user_id = u.id AND a.provider = 'discord'
         WHERE s.session_token = $1 AND s.expires > now()`,
        [sessionToken]
      );
      if (!result.rows[0]) return null;
      const row = result.rows[0];
      return {
        session: {
          sessionToken: row.session_token as string,
          userId: row.user_id as string,
          expires: new Date(row.expires as string),
        },
        user: toAdapterUser(row),
      };
    },

    async updateSession(session) {
      const result = await pool.query(
        `UPDATE sessions SET expires = COALESCE($1, expires)
         WHERE session_token = $2
         RETURNING session_token, user_id, expires`,
        [session.expires, session.sessionToken]
      );
      if (!result.rows[0]) return null;
      const row = result.rows[0];
      return {
        sessionToken: row.session_token as string,
        userId: row.user_id as string,
        expires: new Date(row.expires as string),
      };
    },

    async deleteSession(sessionToken) {
      await pool.query(
        "DELETE FROM sessions WHERE session_token = $1",
        [sessionToken]
      );
    },
  };
}

export const { handlers, auth, signIn, signOut } = NextAuth({
  adapter: pgAdapter(),
  providers: [
    Discord({
      authorization: { params: { scope: "identify" } },
      checks: ["state", "pkce"],
    }),
  ],
  session: { strategy: "database" },
  events: {
    // First login: signIn callback runs BEFORE linkAccount, so the UPDATE
    // in signIn hits 0 rows. This event fires AFTER linkAccount, ensuring
    // provider_name/provider_image are populated on initial account creation.
    async linkAccount({ account, profile }) {
      if (account.provider !== "discord" || !profile) return;
      const discordProfile = profile as DiscordProfile;
      const p = extractDiscordProfile(discordProfile);
      await pool.query(
        `UPDATE accounts SET
          provider_name  = $1,
          provider_image = $2,
          updated_at     = now()
         WHERE provider = $3 AND provider_account_id = $4`,
        [p.name, p.avatarUrl, account.provider, account.providerAccountId]
      );
    },
  },
  callbacks: {
    // Returning logins: account already exists, so this UPDATE succeeds and
    // picks up any Discord profile changes (avatar, display name) since last login.
    async signIn({ account, profile }) {
      if (!account || !profile || account.provider !== "discord") return true;

      const discordProfile = profile as DiscordProfile;
      const p = extractDiscordProfile(discordProfile);

      await pool.query(
        `UPDATE accounts SET
          provider_name  = $1,
          provider_image = $2,
          updated_at     = now()
         WHERE provider = $3 AND provider_account_id = $4`,
        [p.name, p.avatarUrl, account.provider, account.providerAccountId]
      );

      return true;
    },
    async session({ session, user }) {
      session.user.id = user.id;
      return session;
    },
  },
  pages: {
    signIn: "/login",
  },
  trustHost: true,
});
