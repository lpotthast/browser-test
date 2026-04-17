use std::{any::Any, panic::AssertUnwindSafe, sync::Arc};

use chrome_for_testing_manager::{Chromedriver, Session};
use futures_util::FutureExt as _;
use rootcause::Report;
use rootcause::prelude::ResultExt;
use thirtyfour::{ChromeCapabilities, ChromiumLikeCapabilities, WebDriver, error::WebDriverResult};

use crate::scheduler::{BrowserTestExecution, BrowserTestExecutionFuture};
use crate::{BrowserTest, BrowserTestError, BrowserTests, BrowserTimeouts, ElementQueryWaitConfig};

pub(crate) type ChromeCapabilitiesSetup =
    dyn Fn(&mut ChromeCapabilities) -> WebDriverResult<()> + Send + Sync + 'static;

pub(crate) fn browser_test_executions<'a, Context, TestError>(
    chromedriver: &'a Chromedriver,
    visible: bool,
    webdriver_timeouts: Option<&'a BrowserTimeouts>,
    element_query_wait: Option<&'a ElementQueryWaitConfig>,
    chrome_capabilities_setups: &'a [Arc<ChromeCapabilitiesSetup>],
    context: &'a Context,
    tests: BrowserTests<Context, TestError>,
) -> impl Iterator<Item = BrowserTestExecutionFuture<'a>> + 'a
where
    Context: Sync + ?Sized + 'a,
    TestError: ?Sized + 'static,
{
    tests
        .into_vec()
        .into_iter()
        .enumerate()
        .map(move |(test_index, test)| {
            Box::pin(execute_browser_test(
                chromedriver,
                visible,
                webdriver_timeouts,
                element_query_wait,
                chrome_capabilities_setups,
                context,
                test_index,
                test,
            )) as BrowserTestExecutionFuture<'a>
        })
}

#[allow(clippy::too_many_arguments)]
async fn execute_browser_test<Context, TestError>(
    chromedriver: &Chromedriver,
    visible: bool,
    webdriver_timeouts: Option<&BrowserTimeouts>,
    element_query_wait: Option<&ElementQueryWaitConfig>,
    chrome_capabilities_setups: &[Arc<ChromeCapabilitiesSetup>],
    context: &Context,
    test_index: usize,
    test: Box<dyn BrowserTest<Context, TestError>>,
) -> BrowserTestExecution
where
    Context: Sync + ?Sized,
    TestError: ?Sized + 'static,
{
    // We use test_name in the panic report. We need a fallback for the case that `.name()` panics.
    let mut test_name = format!("unnamed test at index {test_index}");
    let result = match AssertUnwindSafe(async {
        test_name = test.name().into_owned();
        let effective_timeouts = resolve_webdriver_timeouts(test.as_ref(), webdriver_timeouts);
        let effective_element_query_wait =
            resolve_element_query_wait(test.as_ref(), element_query_wait);

        tracing::info!("Executing browser test: {test_name}");
        chromedriver
            .with_custom_session(
                |caps: &mut ChromeCapabilities| {
                    configure_chrome_capabilities(caps, visible, chrome_capabilities_setups)
                },
                async |session: &Session| {
                    if let Some(timeout_configuration) = effective_timeouts
                        .map(BrowserTimeouts::into_thirtyfour_timeout_configuration)
                    {
                        session.update_timeouts(timeout_configuration).await?;
                    }

                    let configured_driver: WebDriver;
                    let driver: &WebDriver = if let Some(wait) = effective_element_query_wait {
                        configured_driver =
                            session.clone_with_config(wait.into_thirtyfour_webdriver_config()?);
                        &configured_driver
                    } else {
                        session
                    };

                    test.run(driver, context)
                        .await
                        .map_err(Report::into_dynamic)
                },
            )
            .await
            .context_with(|| BrowserTestError::RunTest {
                test_name: test_name.clone(),
            })
    })
    .catch_unwind()
    .await
    {
        Ok(result) => result,
        Err(payload) => Err({
            let message = panic_payload_message(payload.as_ref());
            tracing::error!("Browser test '{test_name}' panicked: {message}");
            Report::new(BrowserTestError::Panic {
                test_name: test_name.clone(),
                message,
            })
        }),
    };

    BrowserTestExecution { test_index, result }
}

fn resolve_webdriver_timeouts<Context, TestError>(
    test: &dyn BrowserTest<Context, TestError>,
    runner_timeouts: Option<&BrowserTimeouts>,
) -> Option<BrowserTimeouts>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    test.timeouts().or_else(|| runner_timeouts.copied())
}

fn resolve_element_query_wait<Context, TestError>(
    test: &dyn BrowserTest<Context, TestError>,
    runner_wait: Option<&ElementQueryWaitConfig>,
) -> Option<ElementQueryWaitConfig>
where
    Context: Sync + ?Sized,
    TestError: ?Sized,
{
    test.element_query_wait().or_else(|| runner_wait.copied())
}

fn configure_chrome_capabilities(
    caps: &mut ChromeCapabilities,
    visible: bool,
    chrome_capabilities_setups: &[Arc<ChromeCapabilitiesSetup>],
) -> WebDriverResult<()> {
    if visible {
        caps.unset_headless()?;
    }
    for setup in chrome_capabilities_setups {
        setup(caps)?;
    }
    Ok(())
}

fn panic_payload_message(payload: &(dyn Any + Send + 'static)) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        return (*message).to_owned();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "<non-string panic payload>".to_owned()
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use assertr::prelude::*;
    use rootcause::Report;
    use thirtyfour::BrowserCapabilitiesHelper;

    use super::*;

    mod resolve_webdriver_timeouts {
        use super::*;

        struct TimeoutOverrideTest {
            timeouts: Option<BrowserTimeouts>,
        }

        #[async_trait::async_trait]
        impl BrowserTest for TimeoutOverrideTest {
            fn name(&self) -> Cow<'_, str> {
                Cow::Borrowed("timeout override")
            }

            fn timeouts(&self) -> Option<BrowserTimeouts> {
                self.timeouts
            }

            async fn run(&self, _driver: &WebDriver, _context: &()) -> Result<(), Report> {
                Ok(())
            }
        }

        #[test]
        fn uses_test_override_before_runner_default() {
            let runner_timeouts = BrowserTimeouts::builder()
                .script_timeout(Duration::from_secs(10))
                .page_load_timeout(Duration::from_secs(10))
                .implicit_wait_timeout(Duration::from_secs(10))
                .build();
            let test_timeouts = BrowserTimeouts::builder()
                .script_timeout(Duration::from_secs(5))
                .page_load_timeout(Duration::from_secs(5))
                .implicit_wait_timeout(Duration::from_secs(5))
                .build();
            let test = TimeoutOverrideTest {
                timeouts: Some(test_timeouts),
            };

            let resolved = resolve_webdriver_timeouts(&test, Some(&runner_timeouts));

            assert_that!(resolved).is_equal_to(Some(test_timeouts));
        }

        #[test]
        fn falls_back_to_runner_default() {
            let runner_timeouts = BrowserTimeouts::builder()
                .script_timeout(Duration::from_secs(10))
                .page_load_timeout(Duration::from_secs(10))
                .implicit_wait_timeout(Duration::from_secs(10))
                .build();
            let test = TimeoutOverrideTest { timeouts: None };

            let resolved = resolve_webdriver_timeouts(&test, Some(&runner_timeouts));

            assert_that!(resolved).is_equal_to(Some(runner_timeouts));
        }

        #[test]
        fn preserves_unconfigured_default() {
            let test = TimeoutOverrideTest { timeouts: None };

            let resolved = resolve_webdriver_timeouts(&test, None);

            assert_that!(resolved).is_none();
        }
    }

    mod resolve_element_query_wait {
        use super::*;

        struct ElementQueryWaitOverrideTest {
            wait: Option<ElementQueryWaitConfig>,
        }

        #[async_trait::async_trait]
        impl BrowserTest for ElementQueryWaitOverrideTest {
            fn name(&self) -> Cow<'_, str> {
                Cow::Borrowed("element query wait override")
            }

            fn element_query_wait(&self) -> Option<ElementQueryWaitConfig> {
                self.wait
            }

            async fn run(&self, _driver: &WebDriver, _context: &()) -> Result<(), Report> {
                Ok(())
            }
        }

        #[test]
        fn uses_test_override_before_runner_default() {
            let runner_wait = ElementQueryWaitConfig::builder()
                .timeout(Duration::from_secs(10))
                .interval(Duration::from_secs(1))
                .build();
            let test_wait = ElementQueryWaitConfig::builder()
                .timeout(Duration::from_secs(5))
                .interval(Duration::from_millis(500))
                .build();
            let test = ElementQueryWaitOverrideTest {
                wait: Some(test_wait),
            };

            let resolved = resolve_element_query_wait(&test, Some(&runner_wait));

            assert_that!(resolved).is_equal_to(Some(test_wait));
        }

        #[test]
        fn falls_back_to_runner_default() {
            let runner_wait = ElementQueryWaitConfig::builder()
                .timeout(Duration::from_secs(10))
                .interval(Duration::from_millis(500))
                .build();
            let test = ElementQueryWaitOverrideTest { wait: None };

            let resolved = resolve_element_query_wait(&test, Some(&runner_wait));

            assert_that!(resolved).is_equal_to(Some(runner_wait));
        }

        #[test]
        fn preserves_unconfigured_default() {
            let test = ElementQueryWaitOverrideTest { wait: None };

            let resolved = resolve_element_query_wait(&test, None);

            assert_that!(resolved).is_none();
        }
    }

    mod configure_chrome_capabilities {
        use super::*;

        #[test]
        fn applies_visible_mode_before_custom_setup() {
            let custom_setup_called = Arc::new(AtomicUsize::new(0));
            let custom_setup = {
                let custom_setup_called = Arc::clone(&custom_setup_called);
                Arc::new(move |caps: &mut ChromeCapabilities| {
                    assert_that!(caps.is_headless()).is_false();
                    custom_setup_called.fetch_add(1, Ordering::SeqCst);
                    caps.add_arg("--window-size=800,600")
                }) as Arc<ChromeCapabilitiesSetup>
            };
            let mut caps = ChromeCapabilities::new();
            caps.set_headless()
                .expect("setting headless should update capabilities");

            configure_chrome_capabilities(&mut caps, true, &[custom_setup])
                .expect("capability setup should succeed");

            assert_that!(custom_setup_called.load(Ordering::SeqCst)).is_equal_to(1);
            assert_that!(caps.is_headless()).is_false();
            assert_that!(caps.has_arg("--window-size=800,600")).is_true();
        }
    }

    mod panic_payload_message {
        use super::*;

        #[test]
        fn handles_common_payload_types() {
            assert_that!(panic_payload_message(&"static")).is_equal_to("static");
            assert_that!(panic_payload_message(&"owned".to_owned())).is_equal_to("owned");
            assert_that!(panic_payload_message(&42usize)).is_equal_to("<non-string panic payload>");
        }
    }
}
