from ..llm.deepseek import DeepSeekClient


class Heap:
    def __init__(self, client: DeepSeekClient) -> None:
        self.prompts: dict[str, str] = {}
        self.client: DeepSeekClient = client

    def add(self, var_name: str, prompt: str) -> None:
        self.prompts[var_name] = prompt

    def release(self, var_name: str) -> str:
        try:
            response = self.client.send_message(self.prompts[var_name])
            del self.prompts[var_name]
        except Exception as e:
            msg = f"var_name {var_name} does not exist"
            print(e)
            raise ValueError(msg)
        return response
