use crate::config::{Config, EmailProvider, SmtpTls};
use anyhow::{Context, Result};
use async_trait::async_trait;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Mailbox, MultiPart},
    transport::smtp::authentication::Credentials,
};
use std::{sync::Arc, time::Duration};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInEmail {
    pub to: String,
    pub link: String,
    pub code: String,
    pub expires_minutes: u32,
}

impl SignInEmail {
    fn subject(&self) -> &'static str {
        "Sign in to Seiza"
    }

    fn text_body(&self) -> String {
        format!(
            "Sign in to Seiza\n\nOpen this link:\n{}\n\nOr enter this code:\n{}\n\nThis link and code expire in {} minutes and can be used only once. If you did not request this message, you can ignore it.\n",
            self.link, self.code, self.expires_minutes
        )
    }

    fn html_body(&self) -> String {
        format!(
            "<!doctype html><html><body><h1>Sign in to Seiza</h1><p><a href=\"{}\">Continue signing in</a></p><p>Or enter this code:</p><p style=\"font-size:1.5rem;letter-spacing:.2em;font-family:monospace\"><strong>{}</strong></p><p>This link and code expire in {} minutes and can be used only once.</p><p>If you did not request this message, you can ignore it.</p></body></html>",
            escape_html(&self.link),
            escape_html(&self.code),
            self.expires_minutes
        )
    }
}

#[async_trait]
pub trait EmailSender: Send + Sync {
    async fn send_sign_in(&self, email: SignInEmail) -> Result<()>;
}

pub async fn email_sender(config: &Config) -> Result<Arc<dyn EmailSender>> {
    match config
        .email_provider
        .context("email provider is unavailable outside accounts mode")?
    {
        EmailProvider::Smtp => Ok(Arc::new(SmtpEmailSender::from_config(config).await?)),
        EmailProvider::Ses => {
            #[cfg(feature = "aws")]
            {
                Ok(Arc::new(SesEmailSender::from_config(config).await?))
            }
            #[cfg(not(feature = "aws"))]
            {
                anyhow::bail!("SES email delivery requires an AWS-enabled build")
            }
        }
    }
}

struct SmtpEmailSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl SmtpEmailSender {
    async fn from_config(config: &Config) -> Result<Self> {
        let host = config
            .smtp_host
            .as_deref()
            .context("SEIZA_SMTP_HOST is required")?;
        let username = config
            .smtp_username
            .as_deref()
            .context("SEIZA_SMTP_USERNAME is required")?;
        let password_path = config
            .smtp_password_file
            .as_deref()
            .context("SEIZA_SMTP_PASSWORD_FILE is required")?;
        let password = read_secret(password_path).await?;
        let mut builder = match config.smtp_tls {
            SmtpTls::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host),
            SmtpTls::Implicit => AsyncSmtpTransport::<Tokio1Executor>::relay(host),
        }
        .context("configuring authenticated SMTP TLS")?;
        if let Some(port) = config.smtp_port {
            builder = builder.port(port);
        }
        let transport = builder
            .credentials(Credentials::new(username.to_owned(), password))
            .timeout(Some(Duration::from_secs(config.smtp_timeout_seconds)))
            .build();
        let from = config
            .email_from
            .as_deref()
            .context("SEIZA_EMAIL_FROM is required")?
            .parse()
            .context("SEIZA_EMAIL_FROM is not a valid mailbox")?;
        Ok(Self { transport, from })
    }
}

#[async_trait]
impl EmailSender for SmtpEmailSender {
    async fn send_sign_in(&self, email: SignInEmail) -> Result<()> {
        let message = Message::builder()
            .from(self.from.clone())
            .to(email
                .to
                .parse()
                .context("recipient is not a valid mailbox")?)
            .subject(email.subject())
            .multipart(MultiPart::alternative_plain_html(
                email.text_body(),
                email.html_body(),
            ))?;
        self.transport
            .send(message)
            .await
            .context("authenticated SMTP relay rejected the sign-in message")?;
        Ok(())
    }
}

#[cfg(feature = "aws")]
struct SesEmailSender {
    client: aws_sdk_sesv2::Client,
    from: String,
    from_identity_arn: Option<String>,
}

#[cfg(feature = "aws")]
impl SesEmailSender {
    async fn from_config(config: &Config) -> Result<Self> {
        let base = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .load()
            .await;
        let sdk_config = if let Some(role_arn) = config.ses_role_arn.as_deref() {
            let mut builder = aws_config::sts::AssumeRoleProvider::builder(role_arn)
                .configure(&base)
                .session_name("seiza-server-email");
            if let Some(path) = config.ses_role_external_id_file.as_deref() {
                builder = builder.external_id(read_secret(path).await?);
            }
            let provider = builder.build().await;
            aws_config::defaults(aws_config::BehaviorVersion::latest())
                .region(base.region().cloned())
                .credentials_provider(provider)
                .load()
                .await
        } else {
            base
        };
        Ok(Self {
            client: aws_sdk_sesv2::Client::new(&sdk_config),
            from: config
                .email_from
                .clone()
                .context("SEIZA_EMAIL_FROM is required")?,
            from_identity_arn: config.ses_from_identity_arn.clone(),
        })
    }
}

#[cfg(feature = "aws")]
#[async_trait]
impl EmailSender for SesEmailSender {
    async fn send_sign_in(&self, email: SignInEmail) -> Result<()> {
        use aws_sdk_sesv2::types::{Body, Content, Destination, EmailContent, Message};

        let utf8 = |data: String| Content::builder().data(data).charset("UTF-8").build();
        let body = Body::builder()
            .text(utf8(email.text_body())?)
            .html(utf8(email.html_body())?)
            .build();
        let message = Message::builder()
            .subject(utf8(email.subject().to_owned())?)
            .body(body)
            .build();
        let content = EmailContent::builder().simple(message).build();
        let destination = Destination::builder().to_addresses(email.to).build();
        self.client
            .send_email()
            .from_email_address(&self.from)
            .set_from_email_address_identity_arn(self.from_identity_arn.clone())
            .destination(destination)
            .content(content)
            .send()
            .await
            .context("Amazon SES rejected the sign-in message")?;
        Ok(())
    }
}

async fn read_secret(path: &std::path::Path) -> Result<String> {
    let value = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("reading secret file {}", path.display()))?;
    let value = value.trim_end_matches(['\r', '\n']).to_owned();
    if value.is_empty() {
        anyhow::bail!("secret file {} is empty", path.display());
    }
    Ok(value)
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_html_escapes_the_login_url() {
        let email = SignInEmail {
            to: "user@example.com".into(),
            link: "https://example.com/signin?token=a&next=\"bad\"".into(),
            code: "12345678".into(),
            expires_minutes: 10,
        };
        let html = email.html_body();
        assert!(html.contains("a&amp;next=&quot;bad&quot;"));
        assert!(!html.contains("next=\"bad\""));
        assert!(email.text_body().contains("12345678"));
    }
}
