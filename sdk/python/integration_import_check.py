from integrations import McpToolInterceptor, OpenAIEscalationAdapter, RoutedToolExecutor

print("python_integrations_imported=true")
print(f"routed_executor={RoutedToolExecutor.__name__}")
print(f"openai_adapter={OpenAIEscalationAdapter.__name__}")
print(f"mcp_interceptor={McpToolInterceptor.__name__}")
